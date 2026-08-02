# Stir4200MacOS

Tool userspace per macOS (Apple Silicon) per scaricare i dati delle immersioni dai
computer subacquei **Scubapro/Uwatec Galileo Sol / Luna (1ª gen)** attraverso un dongle
a infrarossi **SigmaTel STIr4200** (`USB 066F:4200`), parlando direttamente agli endpoint
USB via **libusb** — senza kext, senza DriverKit, nativo arm64.

## Stato

**Fase 0 — analisi.** Vedi [`ANALYSIS.md`](ANALYSIS.md).

Conclusione chiave dell'analisi: `irda.c` di libdivecomputer delega tutto lo stack IrDA
al sistema operativo (`AF_IRDA`/`SOCK_STREAM` = TinyTP). Su macOS quello stack non esiste,
quindi vanno reimplementati in userspace **IrLAP + IrLMP + IrTTP**. Nessuna
implementazione è ancora iniziata: si attende conferma su linguaggio e licenza.

## Struttura prevista (dal brief §7)

`usb/` (trasporto libusb) · `sir/` (framing async + CRC) · `irlap/` · `irlmp/` · `ttp/`
(TinyTP) · `smart/` (protocollo applicativo Uwatec) · `main`.

## Licenza

Da decidere. La logica di registri/framing deriva dal kernel Linux (GPL-2.0);
libdivecomputer è LGPL-2.1. Proposta: **GPL-2.0** (vedi `ANALYSIS.md`).

## Riferimenti

- Driver Linux `drivers/net/irda/stir4200.c`, `net/irda/wrapper.c` (tag `v4.9`).
- `libdivecomputer` (`src/irda.c`, `src/uwatec_smart.c`).
