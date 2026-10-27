# egui 0.30.0, patched for Klippo

This is egui 0.30.0 from crates.io (https://github.com/emilk/egui, by Emil
Ernerfeldt and the egui contributors, dual-licensed MIT or Apache-2.0; see
LICENSE-MIT and LICENSE-APACHE in the egui repository).

Two changes, both for keyboard, screen-reader, voice-control and UI Automation
users (search for "Klippo patch"):

- `src/menu.rs` (`submenu_button_interaction`): a submenu also opens when its
  button is clicked, not only when the mouse hovers it, so items such as
  Downloads > Speed Limiter > Turn on can be reached.
- `src/widgets/text_edit/builder.rs` and `src/text_selection/accesskit_text.rs`:
  a text box accepts AccessKit's "set value" action (UI Automation's
  ValuePattern), which replaces its text.
