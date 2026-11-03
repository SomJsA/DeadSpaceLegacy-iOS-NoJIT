# Original-game simulator rendering validation

The iOS host remains based on Android integration 9cb5f507. No desktop action mappings are enabled.

0.4.1 restored the active SDL EAGL view before querying the window framebuffer and swapping. Private iPad 9 simulator run 41634593153 visibly reached the original headphone prompt and Select Difficulty using the default composition path. This is startup/menu rendering evidence, not first-chapter acceptance or physical iPad acceptance.

That run rejected both diagnostic normalized touches because the pinned rust-sdl2 push_event conversion does not support Finger events. 0.4.2 uses SDL_PushEvent with a correctly initialized SDL_TouchFingerEvent, preserving the normal mobile polling and coordinate route. This only affects opt-in diagnostic input. The harness waits for a per-command ACK and captures delivered UITouch coordinates separately.

The next acceptance target is difficulty confirmation, menu navigation and a complete visual recording of chapter one until the character can be controlled. Simulator screen video starts before the guest; it has no audio. Native guest drawable samples retain their original orientation and lossless pixels, with frame numbers and capture times. Readback/recording overhead and the simulator software renderer prevent using these runs as physical-device performance evidence.

The encrypted original IPA, dedicated secret, recordings and frame archives stay in the explicitly authorized private test session. Input game bytes never enter Git history or the host IPA. Temporary cloud game input and secret must be deleted when testing ends, after evidence is preserved locally.
