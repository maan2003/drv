// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Adapted from Fuchsia 1e1219e3fac944c9a906aea9646939746b6062b3:
// src/connectivity/wlan/drivers/wlansoftmac/rust_driver/src/lib.rs.
// Native bindings supply callback, MLME and SME futures. The two-phase
// supervision and completion classification follow the upstream server.
// Protocol completion is NOT hardware/DMA cleanup certification.

use anyhow::Error;
use futures::channel::oneshot::{self, Canceled};
use futures::{Future, FutureExt};
use log::{error, info, warn};
use std::pin::Pin;

pub(crate) async fn serve(
    mlme_init_receiver: oneshot::Receiver<()>,
    callbacks: impl Future<Output = Result<(), Error>> + 'static,
    mlme: Pin<Box<dyn Future<Output = Result<(), Error>>>>,
    sme: Pin<Box<impl Future<Output = Result<(), Error>>>>,
) -> Result<(), zx::Status> {
    // Create a oneshot::channel to signal to this executor when WlanSoftmacIfcBridge
    // server exits.
    let (bridge_exit_sender, bridge_exit_receiver) = oneshot::channel();
    // Spawn a Task to host the WlanSoftmacIfcBridge server.
    // Unlike Fuchsia Task, dropping a Tokio JoinHandle detaches its task.
    // JoinSet retains structured cancellation on every return path.
    let mut bridge = tokio::task::JoinSet::new();
    bridge.spawn_local(async move {
        let _: Result<(), ()> = bridge_exit_sender.send(callbacks.await).map_err(|result| {
            error!(
                "Failed to send serve_wlan_softmac_ifc_bridge() result: {:?}",
                result
            )
        });
    });

    let mut mlme = mlme.fuse();
    let mut sme = sme.fuse();

    // oneshot::Receiver implements FusedFuture incorrectly, so we must call .fuse()
    // to get the right behavior in the select!().
    //
    // See https://github.com/rust-lang/futures-rs/issues/2455 for more details.
    let mut bridge_exit_receiver = bridge_exit_receiver.fuse();
    let mut mlme_init_receiver = mlme_init_receiver.fuse();

    info!("Starting MLME and waiting on MLME initialization to complete...");
    // Run the MLME server and wait for the MLME to signal initialization completion.
    //
    // The order of the futures in this select is not arbitrary. During initialization, there is
    // an edge case where MLME could be stopped before initialization completes. By polling
    // the MLME future first, we can unit test handling this edge case by completing the MLME
    // future and initialization, in that order, and then polling the future returned by
    // serve() (i.e., this function).
    {
        futures::select_biased! {
            mlme_result = mlme => {
                match mlme_result {
                    Err(e) => {
                        error!("MLME future completed with error during initialization: {:?}", e);
                        std::mem::drop(bridge);
                        return Err(zx::Status::INTERNAL);
                    }
                    Ok(()) => {

                        // It's possible MLME received a DriverEvent::Stop and returned after
                        // signaling initialization completed and before mlme_init_receiver being
                        // polled. If that's the case, then log a warning that SME never started and
                        // return Ok. Exiting the server in this way should be considered okay
                        // because MLME signaled initialization completed and exited successfully.
                        match mlme_init_receiver.now_or_never() {
                            None | Some(Err(Canceled)) => {
                                error!("MLME future completed before signaling initialization complete.");
                                std::mem::drop(bridge);
                                return Err(zx::Status::INTERNAL);
                            }
                            Some(Ok(())) => {
                                warn!("SME never started. MLME future completed successfully just after initialization.");
                                std::mem::drop(bridge);
                                return Ok(());
                            }
                        }
                    }
                }
            }
            init_result = mlme_init_receiver => {
                match init_result {
                    Ok(()) => (),
                    Err(e) => {
                        error!("MLME dropped the initialization signaler: {}", e);
                        std::mem::drop(bridge);
                        return Err(zx::Status::INTERNAL);
                    }
                }
            },
        }
    }

    info!("Starting SME and WlanSoftmacIfc servers...");

    // Run the SME and MLME servers.
    {
        // This loop-select has two phases.
        //
        // In the first phase, all three futures are running. The first phase will break
        // the loop with an error if any of the following events occurs:
        //
        //   - SME future completes before MLME.
        //   - Any future completes with an error.
        //
        // If the bridge_exit_receiver completes successfully, the MLME and SME futures continue.
        // It's possible for bridge_exit_receiver to complete before MLME because
        // the bridge server exits upon receiving the StopBridgedDriver message while the MLME
        // future consumes the StopBridgedDriver message and responds asynchronously.
        //
        // The first phase ends successfully only if the MLME future completes successfully.
        //
        // The second phase runs the SME future and, if not complete, the
        // bridge_exit_receiver future. The second phase ends with an error if either the
        // SME future or bridge_exit_receiver future return an error.  Otherwise, the
        // second phase ends successfully.
        let mut mlme_future_complete = false;
        loop {
            futures::select! {
                mlme_result = mlme => {
                    match mlme_result {
                        Ok(()) => {
                            info!("MLME shut down gracefully.");
                            mlme_future_complete = true;
                        },
                        Err(e) => {
                            error!("MLME shut down with error: {}", e);
                            break Err(zx::Status::INTERNAL)
                        }
                    }
                }
                bridge_result = bridge_exit_receiver => {
                    // We expect the bridge to shut itself down immediately upon receiving a
                    // StopBridgedDriver message, so it's often the case that the bridge task
                    // will exit before MLME. When the bridge task completes first, both
                    // the `mlme` and `sme` futures should continue to run.
                    match bridge_result {
                        Err(Canceled) => {
                            error!("SoftmacIfcBridge result sender dropped unexpectedly.");
                            break Err(zx::Status::INTERNAL)
                        }
                        Ok(Err(e)) => {
                            error!("SoftmacIfcBridge server shut down with error: {}", e);
                            break Err(zx::Status::INTERNAL)
                        }
                        Ok(Ok(())) => info!("SoftmacIfcBridge server shut down gracefully"),
                    }
                }
                sme_result = sme => {
                    if mlme_future_complete {
                        match sme_result {
                            Err(e) => {
                                error!("SME shut down with error: {}", e);
                                break Err(zx::Status::INTERNAL)
                            }
                            Ok(()) => info!("SME shut down gracefully"),
                        }
                    } else {
                        error!("SME shut down before MLME: {:?}", sme_result);
                        break Err(zx::Status::INTERNAL)
                    }
                }
                complete => break Ok(())
            }
        }
    }
}

/// Native binding of upstream serve_wlan_softmac_ifc_bridge. Callback records
/// keep their originating hardware generation; stop is a retained out-of-band
/// request and cannot be lost because an RX queue is full.
pub(crate) async fn serve_wlan_softmac_ifc_bridge(
    upcalls: std::sync::Arc<std::sync::Mutex<crate::runtime::UpcallQueue>>,
    io: std::sync::Arc<std::sync::Mutex<crate::runtime::HostIo>>,
    mut events: futures::channel::mpsc::Sender<crate::mlme::Event>,
    stop: oneshot::Receiver<()>,
    hardware_exit: oneshot::Receiver<Result<(), zx::Status>>,
    deadline: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    overflow: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), Error> {
    use crate::mlme::{DriverEvent, Event};
    use crate::runtime::Upcall;
    use futures::SinkExt;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let notify = upcalls.lock().unwrap().notify.clone();
    let mut stop = stop.fuse();
    let mut hardware_exit = hardware_exit.fuse();
    let mut ethernet_ticks = tokio::time::interval(Duration::from_millis(1));
    ethernet_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if overflow.load(Ordering::Acquire) {
            anyhow::bail!("Native serving queue overflow");
        }
        let callback = async {
            loop {
                {
                    let mut state = upcalls.lock().unwrap();
                    if state.overflowed {
                        return Err(anyhow::anyhow!("Driver callback queue overflow"));
                    }
                    if let Some(event) = state.queue.pop_front() {
                        if matches!(&event, Upcall::Recv { .. }) {
                            state.raw_queued -= 1;
                        }
                        return Ok((state.epoch.clone(), event));
                    }
                }
                notify.notified().await;
            }
        }
        .fuse();
        // The current Ethernet seam is nonblocking and has no async readiness
        // API. Preserve the existing service cadence without a protocol pump.
        let ethernet = ethernet_ticks.tick().fuse();
        futures::pin_mut!(callback, ethernet);
        let (epoch, event) = futures::select_biased! {
            requested = stop => {
                requested.map_err(|_| anyhow::anyhow!("Callback control closed without stop"))?;
                let epoch = upcalls.lock().unwrap().epoch.clone();
                let (responder, stopped) = oneshot::channel();
                pending.fetch_add(1, Ordering::AcqRel);
                if let Err(error) = events.send(Event {
                    context: epoch.context(Instant::now() + Duration::from_secs(3)),
                    event: DriverEvent::Stop { responder },
                }).await {
                    pending.fetch_sub(1, Ordering::AcqRel);
                    return Err(error.into());
                }
                stopped.await.map_err(|_| anyhow::anyhow!("MLME did not acknowledge stop"))?;
                return Ok(());
            },
            result = hardware_exit => {
                anyhow::bail!("Hardware owner exited before protocol stop: {result:?}");
            },
            _ = ethernet => {
                let frame = {
                    let mut io = io.lock().unwrap();
                    if !io.ethernet.is_link_up() { continue; }
                    io.ethernet.take_transmit()
                        .map_err(|error| anyhow::anyhow!("Ethernet ingress: {error:?}"))?
                };
                let Some(frame) = frame else { continue; };
                let epoch = upcalls.lock().unwrap().epoch.clone();
                (epoch, DriverEvent::EthernetTxEvent(frame.as_bytes().to_vec()))
            },
            callback = callback => {
                let (epoch, upcall) = callback?;
                let event = match upcall {
                    Upcall::Recv { bytes, info } => DriverEvent::WlanRxEvent { bytes, rx_info: info },
                    Upcall::TxResult(tx_result) => DriverEvent::TxResultReport { tx_result },
                    Upcall::ScanComplete { status, scan_id } => DriverEvent::ScanComplete { status, scan_id },
                };
                (epoch, event)
            },
        };
        let end = deadline
            .lock()
            .unwrap()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3));
        pending.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = events
            .send(Event {
                context: epoch.context(end),
                event,
            })
            .await
        {
            pending.fetch_sub(1, Ordering::AcqRel);
            return Err(error.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    type Completion = oneshot::Sender<Result<(), Error>>;

    struct Harness {
        init: oneshot::Sender<()>,
        mlme: Completion,
        sme: Completion,
        callbacks: Completion,
    }

    fn harness() -> (
        Pin<Box<dyn Future<Output = Result<(), zx::Status>>>>,
        Harness,
    ) {
        let (init, initialized) = oneshot::channel();
        let (mlme, mlme_result) = oneshot::channel();
        let (sme, sme_result) = oneshot::channel();
        let (callbacks, callback_result) = oneshot::channel();
        (
            Box::pin(serve(
                initialized,
                async { callback_result.await? },
                Box::pin(async { mlme_result.await? }),
                Box::pin(async { sme_result.await? }),
            )),
            Harness {
                init,
                mlme,
                sme,
                callbacks,
            },
        )
    }

    fn run(future: impl Future<Output = ()>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(std::time::Duration::from_secs(2), future)
                .await
                .unwrap();
        });
    }

    // Ported from the upstream ServeTestHarness tests. The channels replace
    // FIDL endpoints; the same initialization and shutdown races are exercised.
    #[test]
    fn serve_exits_with_error_if_mlme_init_sender_dropped() {
        run(async {
            let (mut future, h) = harness();
            assert!(futures::poll!(&mut future).is_pending());
            drop(h.init);
            assert_eq!(future.await, Err(zx::Status::INTERNAL));
        });
    }

    #[test]
    fn serve_exits_with_error_if_mlme_completes_before_init() {
        for failed in [false, true] {
            run(async {
                let (mut future, h) = harness();
                assert!(futures::poll!(&mut future).is_pending());
                h.mlme
                    .send(if failed {
                        Err(anyhow::anyhow!("MLME"))
                    } else {
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(future.await, Err(zx::Status::INTERNAL));
            });
        }
    }

    #[test]
    fn serve_exits_successfully_if_mlme_completes_just_before_init() {
        run(async {
            let (future, h) = harness();
            h.mlme.send(Ok(())).unwrap();
            h.init.send(()).unwrap();
            assert_eq!(future.await, Ok(()));
        });
    }

    #[test]
    fn serve_exits_with_error_if_mlme_completes_and_init_sender_is_dropped() {
        run(async {
            let (future, h) = harness();
            h.mlme.send(Ok(())).unwrap();
            drop(h.init);
            assert_eq!(future.await, Err(zx::Status::INTERNAL));
        });
    }

    #[test]
    fn serve_exits_with_error_if_sme_shuts_down_before_mlme() {
        for failed in [false, true] {
            run(async {
                let (mut future, h) = harness();
                h.init.send(()).unwrap();
                assert!(futures::poll!(&mut future).is_pending());
                h.sme
                    .send(if failed {
                        Err(anyhow::anyhow!("SME"))
                    } else {
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(future.await, Err(zx::Status::INTERNAL));
            });
        }
    }

    #[test]
    fn serve_exits_with_error_if_mlme_completes_with_error() {
        run(async {
            let (mut future, h) = harness();
            h.init.send(()).unwrap();
            assert!(futures::poll!(&mut future).is_pending());
            h.mlme.send(Err(anyhow::anyhow!("MLME"))).unwrap();
            assert_eq!(future.await, Err(zx::Status::INTERNAL));
        });
    }

    #[test]
    fn serve_exits_with_error_if_sme_shuts_down_with_error() {
        run(async {
            let (mut future, h) = harness();
            h.init.send(()).unwrap();
            assert!(futures::poll!(&mut future).is_pending());
            h.mlme.send(Ok(())).unwrap();
            assert!(futures::poll!(&mut future).is_pending());
            h.sme.send(Err(anyhow::anyhow!("SME"))).unwrap();
            assert_eq!(future.await, Err(zx::Status::INTERNAL));
        });
    }

    #[test]
    fn serve_exits_with_error_if_callbacks_fail() {
        run(async {
            let (mut future, h) = harness();
            h.init.send(()).unwrap();
            assert!(futures::poll!(&mut future).is_pending());
            h.callbacks.send(Err(anyhow::anyhow!("callbacks"))).unwrap();
            assert_eq!(future.await, Err(zx::Status::INTERNAL));
        });
    }

    #[test]
    fn serve_shuts_down_gracefully() {
        for callbacks_first in [false, true] {
            run(async {
                let (mut future, h) = harness();
                h.init.send(()).unwrap();
                assert!(futures::poll!(&mut future).is_pending());
                if callbacks_first {
                    h.callbacks.send(Ok(())).unwrap();
                    tokio::task::yield_now().await;
                    assert!(futures::poll!(&mut future).is_pending());
                    h.mlme.send(Ok(())).unwrap();
                } else {
                    h.mlme.send(Ok(())).unwrap();
                    assert!(futures::poll!(&mut future).is_pending());
                    h.callbacks.send(Ok(())).unwrap();
                }
                assert_eq!(futures::poll!(&mut future), Poll::Pending);
                h.sme.send(Ok(())).unwrap();
                assert_eq!(future.await, Ok(()));
            });
        }
    }

    #[test]
    fn supervisor_exit_cancels_callback_task_instead_of_detaching_it() {
        run(async {
            let (init, initialized) = oneshot::channel();
            let (dropped, drop_observer) = oneshot::channel::<()>();
            let callbacks = async move {
                let _lifetime = dropped;
                futures::future::pending::<Result<(), Error>>().await
            };
            let future = serve(
                initialized,
                callbacks,
                Box::pin(futures::future::pending()),
                Box::pin(futures::future::pending()),
            );
            drop(init);
            assert_eq!(future.await, Err(zx::Status::INTERNAL));
            assert!(drop_observer.await.is_err());
        });
    }
}
