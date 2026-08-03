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

## Aperti da chiarire (dall'analisi, vedi ANALYSIS.md §"Rischi/aperti")

- [x] Numeri di endpoint bulk reali (OUT `0x01`/IN `0x82`) e `bNumEndpoints` (2) — M1.
- [x] `bcdDevice=0.0.8` (M1); `REVID` (CTRL2 & 0x03) = **3** letto a M2.
- [ ] L'header `0x55 0xAA len_lo len_hi` è richiesto anche in SIR? È presente anche in
      ricezione o la RX è uno stream async "nudo"? (M3/M4).
- [x] macOS **non** aggancia lo STIr4200: claim diretto OK, nessun kext da rilasciare (M1).
- [ ] Vettori di test noti per l'FCS CRC-CCITT (unit test, pre-hardware).
- [ ] Parametri IrLAP realmente negoziati dal Galileo e latenza round-trip USB misurata
      in userspace (M5/M6) — dato che decide la fattibilità.
- [ ] Traccia di riferimento Windows x64 (USBPcap + Wireshark) di una sessione di
      download funzionante (brief §9).
