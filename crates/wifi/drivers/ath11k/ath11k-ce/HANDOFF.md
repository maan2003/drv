# CE/HTC handoff

Pinned behavior is Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4` (`ce.[ch]` and `htc.[ch]`). The symbol-level status is in [PORT-MAP.md](PORT-MAP.md).

## Transactional contracts

- A CE send error leaves the source-ring cursor unpublished so the payload can be retried. A CE receive error preserves the status/destination cursor and replenishment ownership; partial per-engine batches retain completed work for delivery before retry.
- `HtcPacketIo::send_htc`, `Transport::send`, and `HtcServiceTransport::send_payload` use `Err` to mean the frame was not firmware-visible and may be retried.
- If the raw CE send rejects an HTC frame, `HtcTransport` restores the endpoint's exact pre-send credit count and emits no frame. The private HTC sequence still advances once, matching Linux; retryability does not imply a byte-identical frame or complete private-state rollback.
- Trailer credit reports are staged until the complete trailer validates. A malformed later record therefore cannot partially add credits or double-count them on retry.

## Credit and lifecycle rules

- Core installs reserved-control endpoint 0 before target-ready, then connects HTT, connects WMI, and sends setup-complete, matching `ath11k_core_start`/`ath11k_htc_init` order.
- With global credit flow enabled, only WMI control endpoints retain per-endpoint credit flow. HTT sets `ATH11K_HTC_CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL`, starts with zero credits, and sends without consuming or waiting for credits.
- WMI receives its ready-message allocation, consumes `ceil((HTC header + payload) / target_credit_size)` credits per frame, returns `NoCredits` when exhausted, and replenishes from valid trailer reports. WCN6750 shadow-register support intentionally reduces the advertised total to one before allocation.

## Accepted source quirks and corrections

- Failed Linux HTC sends consume a sequence number; Rust preserves that behavior while rolling back credits.
- Reserved-control endpoint 0 stores the zero max-message field from Linux's dummy response even though Linux validates with `ATH11K_HTC_MAX_CTRL_MSG_LEN`.
- Linux can apply an early credit record before rejecting a malformed later trailer record. Rust deliberately validates/stages the whole trailer first to preserve retry-safe accounting.
- `wait_target` preserves Linux's ignored credit-allocation-helper failure for an unsupported WMI endpoint count.

## Hardware items still open

- No real CE/HTC exchange has been observed yet. The first Redwood run stopped in `HifPowerUp` while opening VFIO MMIO region 0, before CE allocation, HTC ready, service connects, or credits.
- On the first run that reaches HTC, capture ready credit count/size, control/HTT/WMI endpoint assignments and pipe IDs, HTT/WMI connect flags, setup-complete, credit reports, and CE source/destination/status pointer movement.
- The first hardware path is polling-only because the platform exposes no usable MSI mapping. Confirm CE completion/repost progress without depending on an interrupt wakeup.
