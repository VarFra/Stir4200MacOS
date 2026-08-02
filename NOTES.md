# NOTES.md — scoperte sul comportamento reale dell'hardware

Registro delle scoperte non deducibili dai sorgenti (brief §8). Da compilare mano a mano
che si prova sull'hardware reale. Ogni voce: data, milestone, cosa ci si aspettava, cosa
è successo davvero, come si è verificato.

## Aperti da chiarire (dall'analisi, vedi ANALYSIS.md §"Rischi/aperti")

- [ ] Numeri di endpoint bulk reali (OUT/IN) e `bNumEndpoints` dai descrittori (M1).
- [ ] `bcdDevice` e bit `REVID` (CTRL2) del chip in nostro possesso (M1/M2).
- [ ] L'header `0x55 0xAA len_lo len_hi` è richiesto anche in SIR? È presente anche in
      ricezione o la RX è uno stream async "nudo"? (M3/M4).
- [ ] macOS aggancia lo STIr4200? (`ioreg`/`system_profiler`) e come rilasciarlo (M1).
- [ ] Vettori di test noti per l'FCS CRC-CCITT (unit test, pre-hardware).
- [ ] Parametri IrLAP realmente negoziati dal Galileo e latenza round-trip USB misurata
      in userspace (M5/M6) — dato che decide la fattibilità.
- [ ] Traccia di riferimento Windows x64 (USBPcap + Wireshark) di una sessione di
      download funzionante (brief §9).
