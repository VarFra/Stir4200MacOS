# ANALYSIS.md — Download immersioni Galileo via STIr4200 su macOS

Documento di analisi preliminare richiesto dal brief §4. **Nessuna implementazione
ancora.** Obiettivo: rispondere alle 6 domande con riferimenti puntuali ai sorgenti,
e in particolare stabilire *quanto stack IrDA* dobbiamo reimplementare.

Sorgenti letti (scaricati e analizzati riga per riga):

| Sorgente | Repo / tag | Ruolo |
|---|---|---|
| `drivers/net/irda/stir4200.c` | torvalds/linux `v4.9` | driver del dongle: registri, endpoint, baudrate, TX/RX |
| `net/irda/wrapper.c` + `include/net/irda/wrapper.h`, `crc.h` | torvalds/linux `v4.9` | framing async SIR + FCS |
| `src/irda.c` | libdivecomputer `master` | **transport IrDA — la domanda decisiva** |
| `src/uwatec_smart.c` | libdivecomputer `master` | protocollo applicativo Galileo |
| `src/descriptor.c`, `examples/common.c` | libdivecomputer `master` | discovery, filtro nome, **numero LSAP** |

I riferimenti `file:riga` qui sotto sono a queste versioni.

---

## ⚠️ Sintesi esecutiva (leggere prima di tutto)

Il fatto centrale che il brief chiedeva di verificare (§4.5, §4.6, §8) è questo:

> **`irda.c` di libdivecomputer non implementa nulla dello stack IrDA. Apre un socket
> `AF_IRDA` di tipo `SOCK_STREAM` e delega discovery, connessione, affidabilità e
> flow-control al kernel del sistema operativo** (Linux o Windows).

Conseguenza diretta per noi: su macOS **non esiste** uno stack IrDA. Quindi il
"sottoinsieme minimo" del brief **non è minimo**: per presentarci al Galileo come il
peer che si aspetta, dobbiamo reimplementare in userspace **IrLAP + IrLMP + IrTTP**
(TinyTP), perché `SOCK_STREAM` su `AF_IRDA` = connessione **TinyTP** affidabile e
segmentata. Non c'è scorciatoia via IrCOMM né una via più leggera: la connessione è
verso un **LSAP-SEL numerico diretto (= 1)**, senza lookup IAS per nome.

Questo, sommato al rischio timing di §6 (turnaround IrLAP stretti su round-trip USB
di ~1 ms in userspace non real-time), sposta il baricentro del progetto: le milestone
**M5 (discovery IrLAP)** e **M6 (connessione IrLAP/IrLMP/TinyTP)** sono il vero gate
di fattibilità, molto più del dongle in sé (M1–M4 sono meccanici).

La **buona notizia**: una volta stabilita la connessione TinyTP, il protocollo
applicativo Uwatec Smart è **banale** (scrivi `[cmd|params]`, leggi N byte — vedi §5).
Tutto il peso è nello stack di trasporto.

---

## 1. Endpoint dello STIr4200 e loro uso

Il dongle è un bridge "stupido": nessun framing in hardware (commento `stir4200.c:31-37`).
Espone tre canali:

### Control (endpoint 0) — accesso ai registri
Vendor request codes (`stir4200.c:89-94`):

| Nome | Valore | Uso |
|---|---|---|
| `REQ_WRITE_REG`    | `0x00` | (non usato nel path SIR) |
| `REQ_READ_REG`     | `0x01` | lettura di 1+ registri |
| `REQ_READ_ROM`     | `0x02` | lettura ROM |
| `REQ_WRITE_SINGLE` | `0x03` | scrittura di un singolo registro |

- **Scrittura registro** (`write_reg`, `stir4200.c:194-205`):
  `bmRequestType = OUT | VENDOR | DEVICE`, `bRequest = 0x03 (WRITE_SINGLE)`,
  `wValue = valore`, `wIndex = numero registro`, `wLength = 0` (nessun payload).
- **Lettura registri** (`read_reg`, `stir4200.c:208-218`):
  `bmRequestType = IN | VENDOR | DEVICE`, `bRequest = 0x01 (READ_REG)`,
  `wValue = 0`, `wIndex = numero registro`, `wLength = count`, dati nella fase IN.
  Si possono leggere più registri consecutivi in un colpo (usato per lo stato FIFO).

### Bulk OUT (endpoint 1) — trasmissione
`usb_sndbulkpipe(dev, 1)` (`stir4200.c:724`, e `usb_clear_halt` su di esso a `855`).
Riceve il frame SIR *già wrappato* preceduto dall'header a 4 byte del chip (vedi §4).

### Bulk IN (endpoint 2) — ricezione
`usb_rcvbulkpipe(dev, 2)` (`stir4200.c:858`, urb riempita a `887-890` con buffer da
`STIR_FIFO_SIZE = 4096`). Restituisce byte SIR grezzi wrappati, passati direttamente al
de-wrapper (`stir4200.c:820-823`).

> **Da verificare a M1 (hardware reale):** i numeri di pipe `1` e `2` sono l'assunzione
> del driver Linux. Vanno confermati dai descrittori reali (probabile `EP OUT 0x01`,
> `EP IN 0x82`) prima di cablarli. Confermare anche `bNumEndpoints`, eventuale endpoint
> interrupt non usato, e `bcdDevice` (vedi §6).

---

## 2. Registri per reset, transceiver e baudrate

Mappa registri (`stir4200.c:97-109`): `REG_MODE=1`, `REG_PDCLK=2`, `REG_CTRL1=3`,
`REG_CTRL2=4`, `REG_FIFOCTL=5`, `REG_FIFOLSB=6`, `REG_FIFOMSB=7`, `REG_DPLL=8`,
`REG_IRDIG=9`, `REG_TEST=15`.

### Sequenza di init / cambio velocità
Tutta in `change_speed()` (`stir4200.c:499-558`). Per **SIR a 9600** (M2) la sequenza
esatta di scritture registro è:

| # | Reg | Valore (9600 SIR) | Significato | Righe |
|---|---|---|---|---|
| 1 | `CTRL1` (3) | `0x01` | `CTRL1_SRESET` — reset modulatore | `516` |
| 2 | `DPLL`  (8) | `0x15` | "magia non documentata" per il DPLL | `521` |
| 3 | `PDCLK` (2) | `0x77` | clock per 9600 (tabella sotto) | `526` |
| 4 | `MODE`  (1) | `0x2A` | `NRESET|FASTRX|SIR` (0x02\|0x08\|0x20) | `539` |
| 5 | `CTRL1` (3) | `0x80` | `CTRL1_SDMODE` \| `(tx_power&3)<<1` (tx_power=0) | `544` |
| 6 | `CTRL1` (3) | `0x00` | `(tx_power&3)<<1` (tx_power=0) | `549` |
| 7 | `CTRL2` (4) | `0x20` | `(rx_sensitivity&7)<<5` (rx_sensitivity=1) | `554` |

Per **2400** si aggiunge `MODE_2400 (0x01)` al passo 4 (`stir4200.c:536-537`).
Passi 5–6 resettano un eventuale transceiver stile TEMIC (commento `543`).

Bit del registro `MODE` (`stir4200.c:111-119`): `FIR=0x80 SIR=0x20 ASK=0x10 FASTRX=0x08
FFRSTEN=0x04 NRESET=0x02 2400=0x01`.
Bit `CTRL1` (`131-137`): `SDMODE=0x80 RXSLOW=0x40 TXPWD=0x10 RXPWD=0x08 SRESET=0x01`.
Bit `CTRL2` (`139-142`): `SPWIDTH=0x08 REVID=0x03` (i bit `REVID` = revisione chip).

### Tabella PDCLK per baudrate (`stir4200.c:121-129`)

| Baud | PDCLK |
|---|---|
| 2400   | `0xDF` |
| 9600   | `0x77` |
| 19200  | `0x3B` |
| 38400  | `0x1D` |
| 57600  | `0x13` |
| 115200 | `0x09` |

### Stato / clear FIFO
Lo stato TX/RX si legge come 3 registri consecutivi a partire da `REG_FIFOCTL`
(`fifo_txwait`, `stir4200.c:591-647`): `byte[0]=FIFOCTL`, `byte[1]=FIFOLSB`,
`byte[2]=FIFOMSB`. Conteggio byte in FIFO = `(byte[2] & 0x1f) << 8 | byte[1]`.
Bit `FIFOCTL` (`144-148`): `DIR=0x10` (1 = in trasmissione), `CLR=0x08`, `EMPTY=0x04`.
Svuotamento FIFO: scrivere `FIFOCTL = CLR (0x08)` poi `FIFOCTL = 0x00` (`639-644`).

### Reset a livello USB
- `usb_reset_configuration(dev)` alla probe (`stir4200.c:1040`).
- `usb_clear_halt` su entrambi i bulk all'apertura (`stir4200.c:855-858`).
- Reset logico del modulatore = `CTRL1_SRESET` (passo 1 sopra).

> **Criterio M2 (accettazione):** dopo la scrittura, rileggere i registri e verificare
> che contengano i valori scritti. Attenzione: il commento a `stir4200.c:497` avverte
> che la *scrittura multipla* di registri "non sembra funzionare" → scrivere **uno alla
> volta** con `REQ_WRITE_SINGLE`.

---

## 3. Ricezione: polling, non interrupt

Il dongle **richiede polling** (commento esplicito `stir4200.c:31-37`: *"requires polling
to receive the data"*). Non esiste un endpoint interrupt per i dati.

Modello del driver Linux:
- Una urb bulk IN da 4096 byte viene sottomessa e **ri-sottomessa** nella callback di
  completamento (`stir_rcv_irq`, `stir4200.c:807-842`, resubmit a `833`).
- Cadenza: **~1 ms**, ossia il round-trip USB (commento `stir4200.c:802-806`:
  *"Wakes up every ms (usb round trip) with wrapped data"*). Ad ogni giro il bridge
  restituisce ciò che ha nel FIFO RX (anche 0 byte).
- TX e RX sono **half-duplex**, arbitrati dal bit direzione del FIFO. Prima di trasmettere
  bisogna fermare la RX e rispettare il turnaround (`stir_send`, `stir4200.c:698-708`;
  `turnaround_delay`, `651-668`).

**Traduzione in userspace (libusb):** un loop che chiama `libusb_bulk_transfer(EP_IN,
buf, 4096, timeout)`; su timeout/0 byte si ripete; i byte ottenuti si danno al de-wrapper.
Il vero problema non è il polling in sé ma la **latenza per round-trip** rispetto ai
timer IrLAP (vedi §6).

---

## 4. Framing SIR: BOF/EOF, escaping, FCS

Tutto in `wrapper.c` + costanti in `wrapper.h`/`crc.h`.

### Costanti (`include/net/irda/wrapper.h`)
`BOF = 0xC0`, `EOF = 0xC1`, `CE = 0x7D` (control escape), `XBOF = 0xFF`,
`IRDA_TRANS = 0x20` (modificatore di trasparenza).

### Struttura del frame trasmesso (`async_wrap_skb`, `wrapper.c:83-158`)

```
[ XBOF × N (0xFF) ] [ BOF 0xC0 ] [ dati con byte-stuffing ] [ FCS_lo* FCS_hi* ] [ EOF 0xC1 ]
```

- **XBOF**: N byte di preambolo `0xFF`. Per i frame generati fuori dal layer LAP
  (il nostro caso) il default è **10** (`wrapper.c:103-113`, ramo `wrong magic`),
  clampato a 163 (`117-121`).
- **Byte stuffing** (`stuff_byte`, `wrapper.c:58-75`): se il byte è `BOF`, `EOF` o `CE`,
  emetti `CE (0x7D)` seguito da `byte ^ 0x20`; altrimenti il byte tal quale.
- Il **FCS** è inserito dopo i dati, LSB-first, esso stesso soggetto a stuffing
  (`wrapper.c:146-154`), poi chiude `EOF`.

### FCS (CRC) — attenzione, non è il CRC-16 "standard"
Definizione in `include/net/irda/crc.h`:
- `irda_fcs(fcs, c) = crc_ccitt_byte(fcs, c)` → **CRC-CCITT**, polinomio `0x1021`,
  implementato con **tabella riflessa `0x8408`** (LSB-first).
- `INIT_FCS = 0xFFFF` (inizializzazione).
- In trasmissione il FCS viene **complementato**: `fcs = ~fcs` prima dell'invio
  (`wrapper.c:147`), LSB-first.
- In ricezione, facendo girare il CRC su *dati + FCS ricevuto*, un frame valido dà
  `GOOD_FCS = 0xF0B8` (`wrapper.c:355`, `crc.h`).

Questo è l'FCS-16 tipo HDLC/PPP. **Va testato su vettori noti** (brief §6): p.es. la
stringa ASCII `"123456789"` con questo CRC-CCITT riflesso init 0xFFFF dà `0x6F91`
(valore da confermare nel test, insieme a un frame reale catturato).

### De-wrapping in ricezione (`async_unwrap_char`, `wrapper.c:472-490`)
Macchina a stati `OUTSIDE_FRAME / BEGIN_FRAME / LINK_ESCAPE / INSIDE_FRAME`
(`wrapper.h`): `BOF` inizializza il buffer; `CE` mette in escape (byte successivo
`^0x20`); `EOF` chiude e verifica l'FCS; frame con CRC errato → scartati marcando media
busy, **senza crash** (`wrapper.c:359-366`). È il comportamento richiesto da M4.

### Header proprietario del chip STIr4200 (solo bulk OUT)
Attenzione: prima del frame async, il driver antepone **4 byte di header del chip**
(`wrap_sir_skb`, `stir4200.c:296-308`):

```
[ 0x55 ] [ 0xAA ] [ len_lo ] [ len_hi ] [ frame async (BOF..EOF) ]
```

dove `len` è la lunghezza del frame async wrappato (LE 16 bit). Questo header è **solo
in trasmissione** sul bulk OUT. In ricezione il driver passa i byte del bulk IN
direttamente al de-wrapper, **senza** togliere alcun header (`stir4200.c:820-823`).

> **Da verificare a M3/M4 (hardware):** che l'header `0x55 0xAA len_lo len_hi` sia
> effettivamente richiesto anche in SIR (è documentato per FIR, `stir4200.c:225-238`; per
> SIR lo aggiunge comunque `wrap_sir_skb`) e che la RX sia davvero uno stream async
> "nudo" senza header. È esattamente il tipo di dettaglio da confrontare con una traccia
> USBPcap di riferimento (brief §9).

---

## 5. Domanda decisiva: cosa fa `irda.c` di libdivecomputer

**Non implementa lo stack. Usa i socket IrDA del sistema operativo.**

### Discovery — a livello IrLMP, via il kernel
`dc_irda_iterator_new` (`irda.c:133-257`):
- Apre `socket(AF_IRDA, SOCK_STREAM, 0)` (`irda.c:156`).
- Enumera i dispositivi con `getsockopt(fd, SOL_IRLMP, IRLMP_ENUMDEVICES, ...)`
  (`irda.c:174`). È la **discovery IrLMP** (che sotto usa gli XID di IrLAP), fatta dal
  kernel. Ritenta fino a 4 volte con `sleep(1000ms)` se non trova nulla
  (`irda.c:55-56, 173-204`).
- Per ogni dispositivo legge: **indirizzo a 32 bit** (`daddr`), **nome** (`info`),
  `charset`, `hints` (`irda.c:217-223`). Filtra per **nome del dispositivo** con
  `dc_descriptor_filter(..., DC_TRANSPORT_IRDA, name)` (`irda.c:228`).

### Connessione — a un LSAP-SEL numerico diretto
`dc_irda_open` (`irda.c:283-341`):
- Costruisce `sockaddr_irda` con `sir_addr = address`, `sir_lsap_sel = lsap`, e
  **`sir_name` azzerato con `memset`** (`irda.c:317-322`).
- Fa `connect()` (`irda.c:324`).

`sir_name` vuoto ⇒ la connessione è verso un **LSAP-SEL numerico**, **non** verso un
nome di servizio (niente lookup IAS, niente IrCOMM). Su Windows lo stesso effetto è
ottenuto col nome-magico `"LSAP-SEL%u"` (`irda.c:315`).

### Qual è il LSAP e il nome del Galileo
- **LSAP = 1**: il chiamante fa `dc_irda_open(&iostream, context, address, 1)`
  (`examples/common.c:514`), dopo aver preso l'indirizzo dalla discovery
  (`common.c:493-501`).
- **Nome pubblicizzato** dal Galileo (usato dal filtro discovery): il filtro
  `dc_filter_uwatec` accetta gli IrDA name `"UWATEC Galileo"` e `"UWATEC Galileo Sol"`
  (`descriptor.c:664-665`), con match per prefisso/sottostringa (`dc_match_name`).
  Sol e Luna sono `DC_FAMILY_UWATEC_SMART`, modello `0x11`, transport IRDA
  (`descriptor.c:149-150`).

### Che livello di stack implica `SOCK_STREAM`
Su Linux e Windows, `AF_IRDA` + `SOCK_STREAM` = connessione **IrTTP (TinyTP)**:
affidabile, con segmentazione (SAR) e **flow-control a crediti**, sopra IrLMP sopra
IrLAP. Quindi **il Galileo si aspetta un peer che parli TinyTP**, non solo IrLAP.

### Protocollo applicativo (una volta su lo stream TTP) — è banale
`uwatec_smart_irda_send/receive` (`uwatec_smart.c:87-152`): sopra lo stream affidabile
non c'è né framing né checksum applicativo (li fornisce TTP/LAP). Si **scrive
`[cmd | params]`** e si **legge esattamente N byte**. Comandi (`uwatec_smart.c:39-48`):

| Comando | Byte | Parametri | Risposta |
|---|---|---|---|
| `HANDSHAKE1` | `0x1B` | — | 1 byte = `OK 0x01` |
| `HANDSHAKE2` | `0x1C` | `{0x10,0x27,0,0}` | 1 byte = `OK 0x01` |
| `MODEL`      | `0x10` | — | 1 byte (id modello, Galileo = `0x11`) |
| `HARDWARE`   | `0x11` | — | 1 byte |
| `SOFTWARE`   | `0x13` | — | 1 byte |
| `SERIAL`     | `0x14` | — | 4 byte LE |
| `DEVTIME`    | `0x1A` | — | 4 byte LE (clock dispositivo) |
| `SIZE`       | `0xC6` | 4B timestamp + `{0x10,0x27,0,0}` | 4 byte LE = lunghezza dati |
| `DATA`       | `0xC4` | idem | 4 byte LE (=len+4) poi `len` byte |

(handshake: `uwatec_smart.c:425-459`; dump/lettura memoria: `557-685`.)
I dati immersioni contengono immersioni delimitate dal marker
`A5 A5 5A 5A` seguito da 4 byte LE di lunghezza (`uwatec_smart.c:716-738`).
`{0x10,0x27,0,0}` = `0x2710` = 10000. Il primo campo dei params di `SIZE`/`DATA` è il
timestamp/fingerprint (`device->timestamp`) per scaricare solo le immersioni più recenti.

---

## 6. Sottoinsieme minimo di IrLAP/IrLMP/TinyTP da implementare

Poiché (§5) il device pretende un peer **TinyTP su LSAP 1**, il "minimo" reale è un
piccolo stack SIR completo fino a TinyTP. Frame che dobbiamo saper **costruire e
interpretare**:

### IrLAP (link access)
- **Discovery**: `XID command` (broadcast, a slot) e parsing di `XID response`
  (indirizzo device 32 bit, hint bits, charset, nickname). Gestione dello slotting e
  del generation address.
- **Connessione**: `SNRM` (Set Normal Response Mode) con **negoziazione parametri (PV)** —
  baud rate, max turnaround time, data size, window size, additional BOFs, min turn
  time, link disconnect/threshold time — e risposta `UA` (Unnumbered Ack).
- **Trasferimento informazioni**: `I-frame` con numeri di sequenza `N(s)/N(r)`,
  supervisory `RR`/`RNR`/`REJ`, e gestione rigorosa del bit **P/F (poll/final)** e del
  turnaround (qui vive il rischio timing di sotto).
- **Disconnessione**: `DISC` / `UA` / `RD`.
- Byte di indirizzo/controllo e assegnazione dell'indirizzo di connessione (7 bit).

### IrLMP (link management / multiplexing)
- Header LM-PDU: `{DLSAP-SEL, control bit, SLSAP-SEL}` + payload.
- **Connect**: `CONNECT (0x01)` / `CONNECT_CONFIRM` verso `DLSAP = 1`; `DISCONNECT (0x02)`.
- PDU dati.
- **IAS non necessario**: connettiamo a LSAP fisso 1, niente query "IrLMP:LsapSel".
  (La discovery è pilotata da IrLAP.)

### IrTTP / TinyTP (obbligatorio perché `SOCK_STREAM`)
- **Connect** con parametri: max SDU size e **credito iniziale**.
- **Dati** con header TTP a 1 byte: bit 7 = `More` (segmentazione SAR), bit 6-0 =
  `delta credit` (flow-control a crediti). Va gestito il rifornimento crediti.
- **SAR**: la segmentazione va gestita almeno per il download dati (potenzialmente vari
  KB), anche se i comandi corti stanno in un singolo segmento.

### Verdetto di fattibilità (richiesto da §4.6 e §8)
Lo stack necessario è **decisamente più ampio del "singolo file portato"** ipotizzabile
dal solo `stir4200.c`. Non è enorme (SIR punto-punto, un solo servizio, un solo peer),
ma è un vero stack a tre livelli con macchine a stati e timing stretto. **Il progetto è
plausibile ma il gate è M5–M6**, non il dongle. Raccomandazione operativa allineata a §6:
arrivati a M4, prima di scrivere IrLAP misurare la **latenza reale round-trip USB** e
confrontarla con i timeout IrLAP negoziabili; se il round-trip in userspace è troppo alto
rispetto ai turnaround, negoziare parametri IrLAP permissivi (min turn time alto, window
piccola) e valutare un thread RX ad alta priorità — e, se resta insufficiente,
**fermarsi e riportare il dato misurato** invece di accumulare workaround.

---

## Rischi/aperti da chiarire su hardware (per NOTES.md)

1. Numeri di endpoint bulk reali e `bcdDevice`/`REVID` del chip (§6 brief): i valori
   registro potrebbero non valere per tutte le revisioni.
2. Presenza dell'header `0x55 0xAA len len` in SIR anche in **ricezione** (il driver non
   lo toglie): confermare con traccia reale.
3. Se il kernel macOS aggancia lo STIr4200 (improbabile: nessuno stack IrDA moderno su
   macOS, device vendor-specific) — verificare con `ioreg`/`system_profiler` e gestire
   l'eventuale claim prima di usarlo con libusb.
4. Vettori di test noti per l'FCS CRC-CCITT (da fissare come unit test, brief §6/§7).
5. **Traccia di riferimento Windows x64 (USBPcap + Wireshark, brief §9): è la cosa a più
   alto valore.** Una singola sessione di download catturata risolve in un colpo header
   del chip, sequenza init reale, parametri IrLAP negoziati e timing — più di qualsiasi
   ipotesi.

---

## Proposte che richiedono una tua decisione (§7)

Non sono parte dell'analisi tecnica ma servono prima di scrivere codice:

- **Linguaggio**: propongo **Rust** (buffer safety su parsing non fidato, ottimo per le
  macchine a stati IrLAP/TTP, `rusb` come binding libusb sottile), *tenendo* il framing
  SIR + CRC come modulo isolato e testato a parte. Alternativa **C**, che rende il
  confronto 1:1 col sorgente Linux più diretto. Decisione tua.
- **Licenza**: la logica di registri/framing è derivata dal kernel Linux **GPL-2.0**;
  libdivecomputer è **LGPL-2.1**. Per coerenza propongo **GPL-2.0** per l'intero progetto,
  documentata nel README. Decisione tua.
- **Formato export (M8)**: valuto più robusto **produrre il dump grezzo della memoria** e
  farlo digerire da libdivecomputer/Subsurface, piuttosto che generare XML noi. Da
  definire a M8, non ora.

---

## STOP (come da brief §4)

Analisi completata. **Mi fermo qui e attendo conferma** prima di procedere con
l'implementazione. Le due decisioni che mi servono per partire con M1 sono
**linguaggio** e **licenza**; e la conferma che, vista l'estensione reale dello stack
(§5–§6), vuoi comunque procedere per milestone partendo da M1 (enumerazione USB).
