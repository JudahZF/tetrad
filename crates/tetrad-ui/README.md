# tetrad-ui

The owned touch UI for the 800×480 display: the screen framework (navigation, gestures, lists,
keyboard, dialogs, knob and footswitch routing), every screen, the boot selector frame and the
view models that the application fills. It draws with `tetrad-assets`. Commands are requests, not
confirmations of a load or a save. No display device is opened here.

```rust
use tetrad_ui::{Control, InputEvent, Screen, UiSnapshot, UiState};

let mut ui = UiState::new();
ui.set_snapshot(UiSnapshot::default()).unwrap();
ui.handle_input(InputEvent::Activate(Control::Navigate(Screen::Gig)));
ui.handle_input(InputEvent::Tick { now_ms: 16 });
```

`examples/render_owned.rs` renders a synthetic state to a PPM image on stdout, and writes the
README images (see [docs/development.md](../../docs/development.md#readme-images)).

Part of [Tetrad](../../README.md).
