# Redwood native ath11k oracle

The pre-port capability capture found that the running 7.2.0 kernel was built
with `CONFIG_ATH11K_TRACING` unset. Consequently no ath11k tracefs events
exist, and `trace-cmd` is absent. No radio cycle was attempted: doing so could
not produce the requested oracle and would unnecessarily drop the only link.

`scripts/redwood/capture-native-ath11k` fails closed on those prerequisites
and structures a future binary trace by WMI and HTT. Linux's tracepoints include
exact dynamic WMI command/event arrays and selected HTT pktlog/PPDU/RX
descriptor arrays. They do **not** expose QMI payloads or general TCL/REO/WBM
descriptors; those require a narrow temporary kernel tracepoint/DMA snapshot,
not a claim that ftrace observed them.

The tracing kernel must be tested by kexec only under the separately proven
hardware-watchdog lease. Its report remains local until native Wi-Fi returns.
