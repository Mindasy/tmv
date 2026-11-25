# TMV 0.2.0

Targets openEuler 24.03 LTS on aarch64 and x86_64. Each architecture must pass
package acceptance before publication; compilation alone is not a compatibility claim.

- Preserves process memory layout, treemap, page state and memory inspection.
- Bundles the Linux/glibc LD_PRELOAD allocation collector.
- Adds run/attach, offline open/report, allocation labels and call stacks.
- Offline replay includes time selection, memory trends and address block maps.
- Terminal views support mouse selection, dragging, scrolling and navigation.

The collector targets same-architecture 64-bit dynamically linked glibc programs.
Static executables and secure-execution targets are not supported by LD_PRELOAD.
Recorded playback is sampled, not an exact replay of every allocation event.
Historical memory bytes are not captured. Attaching without prior instrumentation
does not reconstruct past allocations. Recording stacks has runtime overhead.

Unpack the full archive and run ./tmv; the adjacent libtmvtrace.so is discovered
automatically. RPM installs it in /usr/lib64/tmv/. No global preload is configured.
The binary RPM installs no examples. In the portable archive, open the bundled recording with: ./tmv open examples/demo.tmvr --at 9

For native package builds and acceptance see tools/release/README.md.
