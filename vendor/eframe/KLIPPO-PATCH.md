# eframe 0.30.0, patched for Klippo

This is eframe 0.30.0 from crates.io (https://github.com/emilk/egui, by Emil
Ernerfeldt and the egui contributors, dual-licensed MIT or Apache-2.0; see
LICENSE-MIT and LICENSE-APACHE in the egui repository).

One change, in `src/native/glow_integration.rs`: eframe 0.30 gives only the
main window an AccessKit adapter. The patch keeps the event-loop proxy and
creates an adapter for every window made after start-up (Klippo's dialogs),
so screen readers and Windows UI Automation can read and press their controls.
Search for "Klippo patch" to find the lines.
