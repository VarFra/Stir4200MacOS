# NOTES.md — scoperte sul comportamento reale dell'hardware

Registro delle scoperte non deducibili dai sorgenti (brief §8). Da compilare mano a mano
che si prova sull'hardware reale. Ogni voce: data, milestone, cosa ci si aspettava, cosa
è successo davvero, come si è verificato.

## M1 — Enumerazione (verificato su hardware, macOS Apple Silicon)

Dongle reale, output di `stir4200 -v`:

- `idVendor=0x066f idProduct=0x4200`, **`bcdDevice=0.0.8`** (raw `0x0008`), `bcdUSB=1.1`.
- `bDeviceClass=0xff` (vendor-specific), `bDeviceSubClass=0x01`, `bDeviceProtocol=0x00`.
- `iManufacturer=" Sigmatel Inc "`, `iProduct=" IrDA/USB Bridge"` (con spazi, come da ROM).
- 1 configurazione, 1 interfaccia (n. 0, alt 0), **2 endpoint**:
  - `0x01` bulk **OUT**, max_packet **64** → conferma l'assunzione "bulk OUT ep 1".
  - `0x82` bulk **IN**, max_packet **64** → conferma l'assunzione "bulk IN ep 2".
  - `max_power=440 mA` riportato (alto, ma solo dichiarato nel descrittore).
- **macOS non aggancia il device**: `set_auto_detach_kernel_driver` ok e
  `claim_interface(0)` riesce senza dover fare unload di alcun kext. → il punto 3
  dei "rischi aperti" è risolto: nessun driver di sistema da rilasciare.
- `bcdDevice=0.0.8` è la revisione contro cui è stato scritto il driver Linux: i valori
  dei registri (§2 ANALYSIS) dovrebbero applicarsi. Da confermare a M2 leggendoli indietro.

Conseguenze pratiche per i livelli successivi: endpoint bulk `0x01`/`0x82`, pacchetti USB
da 64 byte, transfer di controllo sull'endpoint 0 per i registri.

## M2 — Init e registri (verificato su hardware)

`stir4200 init -v` a 9600 SIR. Sequenza di scrittura eseguita senza errori USB
(reg 3=0x01, 8=0x15, 2=0x77, 1=0x2a, 3=0x80, 3=0x00, 4=0x20). Read-back:

- **PDCLK (reg 2)**: scritto `0x77`, riletto `0x77` → OK (registro pienamente leggibile/scrivibile).
- **MODE (reg 1)**: scritto `0x2a`, riletto `0x2a` → OK.
- **CTRL2 (reg 4)**: scritto `0x20`, riletto **`0x27`**. Non è un errore:
  - i 3 bit alti (`0xE0`, campo *rx sensitivity*) rileggono `0x20` = valore scritto → OK;
  - i bit bassi sono **read-only**: `CTRL2_REVID (0x03)` = **revisione chip 3**, più il
    bit `0x04` acceso (stato read-only non nominato nell'enum del driver; `SPWIDTH=0x08`
    è spento). Quindi `0x27 = 0x20 (scritto) | 0x07 (read-only)`.
  - **Lezione**: verificare solo i **bit scrivibili** di ogni registro. Il codice ora
    confronta con una maschera (`CTRL2` → `0xE0`). Con questa correzione M2 è **OK**.
- **FIFO status (reg 5-7, lettura multi-registro)**: `ctl=0x04` = `FIFOCTL_EMPTY`,
  direzione RX, `count=0`. Stato idle corretto; il path di lettura a 3 registri (l'unico
  che il driver Linux usa davvero) **funziona**.

**Conclusione M2**: meccanismo di I/O sui registri pienamente funzionante, baudrate
impostato, FIFO leggibile. `REVID = 3` per questo esemplare (bcdDevice 0.0.8).

## M3 — Trasmissione grezza (verificato su hardware)

`stir4200 tx -v` a 9600 SIR, 100 frame: **nessun errore USB** e **sfarfallio IR
visibile** con la fotocamera dello smartphone. Il frame sul filo (payload di test
`00 c0 c1 7d ff 55 aa`) è esattamente quello atteso (28 byte totali):

```
55 aa 18 00                          header chip (0x55 0xAA, len=0x0018=24)
ff ff ff ff ff ff ff ff ff ff        10 XBOF
c0                                   BOF
00  7d e0  7d e1  7d 5d  ff  55  aa  payload con escaping (C0/C1/7D → CE + b^0x20)
16 7b                                FCS (LSB-first)
c1                                   EOF
```

→ Header del chip, conteggio XBOF, byte-stuffing, campo lunghezza e FCS **tutti
corretti in trasmissione**. L'header `0x55 0xAA len len` è quindi richiesto/accettato
anche in SIR (resta da vedere la RX a M4).

## M4 — Ricezione grezza (verificato su hardware)

`stir4200 rx -v` a 9600: con un **telecomando TV** avvicinato, arrivano **1328 byte
grezzi**, 0 frame validi, 0 errori CRC, **nessun crash**. → path di RX funzionante e
scarto dei frame malformati OK. Criterio M4 soddisfatto.

Scoperte sul comportamento reale della RX:

- Il bulk IN restituisce i dati in **transfer molto piccoli (1 byte alla volta)** in questo
  scenario, non in blocchi. Va bene per il loop di polling; da tenere presente per il
  timing di M5/M6.
- Dal telecomando arrivano prevalentemente byte **`0xFF`**. In SIR `0xFF = XBOF`, quindi il
  de-wrapper li ignora correttamente come "fuori frame" (nessun frame spurio).
- **Non è emerso alcun header/byte di stato del chip in ricezione**: lo stream RX sembra
  essere async "nudo" (coerente col driver Linux che passa i byte del bulk IN direttamente
  al de-wrapper). Da riconfermare con frame veri IrDA a M5.

**Il computer subacqueo non produce byte da solo** (confermato dal manuale: l'interfaccia
IR del Galileo si attiva solo quando *sente* una trasmissione). → è **atteso**: il Galileo
risponde alla discovery IrLAP (XID), non trasmette spontaneamente. La ricezione dal
dispositivo si potrà verificare solo da M5 in poi (dopo aver inviato noi la discovery).
Implica che M5 deve fare **TX (XID) → turnaround → RX (risposta)** in half-duplex.

## Aperti da chiarire (dall'analisi, vedi ANALYSIS.md §"Rischi/aperti")

- [x] Numeri di endpoint bulk reali (OUT `0x01`/IN `0x82`) e `bNumEndpoints` (2) — M1.
- [x] `bcdDevice=0.0.8` (M1); `REVID` (CTRL2 & 0x03) = **3** letto a M2.
- [x] L'header `0x55 0xAA len_lo len_hi` è richiesto in SIR **TX** (confermato M3). In
      **RX** lo stream è async "nudo", nessun header del chip osservato (M4; riconfermare
      con frame IrDA veri a M5).
- [x] macOS **non** aggancia lo STIr4200: claim diretto OK, nessun kext da rilasciare (M1).
- [ ] Vettori di test noti per l'FCS CRC-CCITT (unit test, pre-hardware).
- [ ] Parametri IrLAP realmente negoziati dal Galileo e latenza round-trip USB misurata
      in userspace (M5/M6) — dato che decide la fattibilità.
- [ ] Traccia di riferimento Windows x64 (USBPcap + Wireshark) di una sessione di
      download funzionante (brief §9).
