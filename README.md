# Stir4200MacOS

Tool userspace per macOS (Apple Silicon) per scaricare i dati delle immersioni dai
computer subacquei **Scubapro/Uwatec Galileo Sol / Luna (1ª gen)** attraverso un dongle
a infrarossi **SigmaTel STIr4200** (`USB 066F:4200`), parlando direttamente agli endpoint
USB via **libusb** — senza kext, senza DriverKit, nativo arm64.

## Stato

- **Fase 0 — analisi**: completata. Vedi [`ANALYSIS.md`](ANALYSIS.md).
- **M1 — Enumerazione USB**: ✅ verificato su hardware (vedi [`NOTES.md`](NOTES.md)).
- **M2 — Init e registri**: ✅ verificato su hardware (baudrate 9600, registri OK).
- **M3 — Trasmissione grezza**: ✅ verificato su hardware (frame corretto sul filo, LED IR emette).
- **M4 — Ricezione grezza**: ✅ verificato su hardware (byte ricevuti, nessun crash).
- **M5 — Discovery IrLAP**: ✅ verificato su hardware (il Galileo risponde: "UWATEC Galileo").
- **M6 — Connessione IrLAP (SNRM/UA + keepalive)**: ✅ verificato (382/382 poll, gap 0 — timing risolto).
- **M7 — Protocollo Uwatec Smart (IrLMP+TinyTP)**: ✅ verificato (scaricati 287612 byte su file).
- **M8 — Parsing ed esportazione per Subsurface**: codice scritto (`stir4200 parse`), **in attesa di verifica**.

Conclusione chiave dell'analisi: `irda.c` di libdivecomputer delega tutto lo stack IrDA
al sistema operativo (`AF_IRDA`/`SOCK_STREAM` = TinyTP). Su macOS quello stack non esiste,
quindi vanno reimplementati in userspace **IrLAP + IrLMP + IrTTP**.

Linguaggio: **Rust** (binding libusb `rusb`). Licenza: **GPL-2.0-only**.

## Build ed esecuzione

```sh
cargo build --release          # libusb è compilato staticamente (feature "vendored")
cargo test                     # unit test SIR/CRC, non richiedono hardware
./target/release/stir4200          # M1: enumera il dongle 066F:4200
./target/release/stir4200 -v       # con logging di debug (hex dump dei frame)
./target/release/stir4200 init -v  # M2: reset + baudrate 9600 + rilettura registri
./target/release/stir4200 tx -v    # M3: trasmette frame SIR di test (LED IR visibile con fotocamera)
./target/release/stir4200 rx -v        # M4: ascolta sul bulk IN e de-wrappa lo stream SIR
./target/release/stir4200 discover -v  # M5: discovery IrLAP (XID) del computer subacqueo
./target/release/stir4200 connect -v   # M6: connessione IrLAP (SNRM/UA) + keepalive 30s
./target/release/stir4200 download -o dump.bin  # M7: scarica la memoria immersioni su file
./target/release/stir4200 parse -i dump.bin -o dives.xml  # M8: converte il dump in XML Subsurface
```

Su macOS, se il dispositivo non viene trovato, controllare che sia collegato con
`system_profiler SPUSBDataType` o `ioreg -p IOUSB`.

### Criterio di accettazione M1

`stir4200` apre `066F:4200`, ne rivendica l'interfaccia e stampa l'albero dei
descrittori (device, configurazioni, endpoint), segnalando se gli endpoint bulk
corrispondono a quelli attesi dal driver Linux (OUT ep 1, IN ep 2). Il `bcdDevice`
viene stampato in evidenza per verificare l'applicabilità dei valori dei registri (§6).

## Struttura (dal brief §7)

`src/usb/` (trasporto libusb, M1) · `src/sir/` (framing async + CRC; per ora solo il
CRC-CCITT con unit test) · `src/logging.rs` (verbosità + hex dump) · `src/main.rs` (CLI).
`src/irlap/` (discovery XID, connessione SNRM/UA, I-frame/RR NRM primario) ·
`src/smart/` (IrLMP + TinyTP con flow-control a crediti, protocollo Uwatec Smart).

## Licenza

Da decidere. La logica di registri/framing deriva dal kernel Linux (GPL-2.0);
libdivecomputer è LGPL-2.1. Proposta: **GPL-2.0** (vedi `ANALYSIS.md`).

## Riferimenti

- Driver Linux `drivers/net/irda/stir4200.c`, `net/irda/wrapper.c` (tag `v4.9`).
- `libdivecomputer` (`src/irda.c`, `src/uwatec_smart.c`).
