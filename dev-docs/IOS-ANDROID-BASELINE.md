# iOS no-JIT: Android integration baseline

The iOS host is based on Android integration commit `9cb5f50775a590c639aabf47907845f06b300e88`, not on the PC research worktree. The source worktree is `codex/ios-android-base-20261103`.

The CPU interpreter is a shared component from `315ef93e`; the dispatcher omits the PC performance module. Android C++ fastmem and skin-math code stays intact and remains optional. iOS builds only the interpreter backend.

Android window/GLES/HLE files are retained and edited by small platform-specific hunks. UIKit owns one SDL event loop; support libraries remain archive data outside the code signature. Resources and saves resolve to separate absolute Library and Documents paths. Apple framebuffer, BGRA and limited-NPOT requirements are handled explicitly.

On iOS the existing Android multi-touch dispatch remains the gameplay input path. Keyboard/mouse/joystick/controller action events are discarded before desktop remapping; controller enumeration and the desktop virtual cursor are disabled. Missing guest data opens a mobile setup view. The desktop picker and PC launch profiles are not used.

The unchanged guest is copied separately to Documents/DeadSpace.ipa. The host does not ship, modify, sign or replace it. Android CJK fonts, embedded native text bank and mobile default graphics options are preserved. Java overlays/settings have no direct UIKit equivalent yet: their readiness remains false so the original guest UI remains visible. This is a pending mobile UI port, not evidence of full Android UI parity.

Local evidence: 45 interpreter semantics tests and 7 resource/path tests passed; offline project guard passed 21 checks. A Windows full source-check attempt stopped in native CMake due long-path file tracking; it is not an Apple build result. Native Apple compilation, simulator landing and actual iPad gameplay are separate evidence stages. No gameplay or performance acceptance is inferred from the setup view.
