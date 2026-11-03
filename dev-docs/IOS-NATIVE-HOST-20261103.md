# Native iOS no-JIT host candidate

Candidate 0.3.0 build 5, separate bundle `dev.somjs.deadspacelegacy.nojithost`.
This is the complete interpreter HLE core with an SDL UIKit entry, not a new instruction probe.
Source base: bd5b416e; CPU implementation: 315ef93e. PC and Android integration trees stay separate.

The SDL callback runs the ordinary HLE entry on UIKit's main thread. With an unchanged
`Documents/DeadSpace.ipa` it attempts that guest; otherwise it opens the full app picker.
`Documents/host-log.txt` is append-only. HLE user data is rooted in this app's Documents;
setup creates no game or saves and replaces no existing user file.

UIKit's drawable has a nonzero framebuffer. The adapter queries SDL's actual UIKit
window data, binds that framebuffer for composition, and temporarily binds the native
color renderbuffer during swap, restoring the previous binding afterwards.

ARM32 HLE support dylibs are data inside `host-resources.zip`, not native embedded code.
They and fonts unpack into a hash-addressed private Library directory. Traversal,
duplicate entries, nonregular files, oversize archives and overwrite attempts fail.
No original game IPA is bundled or modified. No JIT or extra-memory entitlement is requested.

Evidence before integrated app build: remote probe 0.1.0 on iPad12,1 reports iPadOS 27.2,
16 KiB pages, six CPU vectors and 4 GiB RW virtual mapping PASS. Apple SDK core check and
45 interpreter tests passed in CI run 41588642370. Four resource-cache tests passed on Windows;
21 offline source guard checks passed. These results do not certify this complete host app.

The integrated workflow builds ARM64 device and simulator apps, checks Mach-O/platform/segments,
packages an unsigned device IPA, and launches the full app picker in an isolated iPad simulator.
Its screenshot, process state and host log are retained. This cannot validate real-device game
play, audio, touch, saves, lifecycle, performance or physical memory pressure without the actual guest.

CI policy: private repository; manual dispatch only; standard macos-15; one job; 20-minute cap;
no automatic retries; included allowance only. User confirmed stop-on-excess and no payment method.
Group changes locally and run one integrated build to conserve the free allowance.

## Startup integration fixes

SDK builds and an actual simulator run exposed SDLUIKitDelegate resetting cwd to the
.app resource path after initialize(). The support archive was complete; relative font
lookups were therefore wrong. iOS ResourceFile now resolves against the configured absolute
resource cache; the SDL callback also restores cwd for the remaining legacy file paths.
The regression actually changes cwd and reads the intended cached font. Cache/path tests: 7.
Core Animation composition, EAGL presentation and optional ES2 presentation all select the
current SDL UIKit framebuffer. Native color-renderbuffer binding is restored after swap.
The packaging audit checks all 18 declared fonts and 9 guest support libraries before compiling.
Simulator acceptance now also rejects an all-black image and retains textual startup logs.

Build 4 passed compilation and a coarse nonblack check, but manual screenshot review
rejected its white surface with a gray cursor. The composited 320x480 texture still
used default REPEAT wrapping. APPLE_texture_2D_limited_npot specifies that this makes
NPOT sampling act untextured. Build 5 sets both axes to CLAMP_TO_EDGE and checks the
composition FBO. Screenshot acceptance requires visible chromatic content, not only
a nonblack surface. The iOS BGRA upload path also follows Apple's RGBA-internal rule.
References:
- https://registry.khronos.org/OpenGL/extensions/APPLE/APPLE_texture_2D_limited_npot.txt
- https://registry.khronos.org/OpenGL/extensions/APPLE/APPLE_texture_format_BGRA8888.txt
