// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Adapted from Fuchsia 1e1219e3fac944c9a906aea9646939746b6062b3:
// src/connectivity/wlan/lib/mlme/rust/src/lib.rs.
// The owning MLME loop is retained. Native bindings replace FFI frames and
// attach publication authority at request/event/timer emission.

use crate::runtime::{MlmeExecution, ScanOperation};
use crate::{OperationContext, OperationEpoch};
use anyhow::{Error, bail, format_err};
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_softmac as fidl_softmac;
use futures::channel::{mpsc, oneshot};
use futures::{StreamExt, select};
use log::info;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use wlan_common::{self as common, sink::UnboundedSink};
use wlan_mlme::{MinstrelWrapper, MlmeImpl, device::DeviceOps};

pub(crate) struct Request {
    pub context: OperationContext,
    pub scan: Option<OperationContext>,
    pub request: wlan_sme::MlmeRequest,
}

pub(crate) enum DriverEvent {
    Stop {
        responder: oneshot::Sender<()>,
    },
    ScanComplete {
        status: zx::Status,
        scan_id: u64,
    },
    TxResultReport {
        tx_result: fidl_softmac::WlanTxResult,
    },
    EthernetTxEvent(Vec<u8>),
    WlanRxEvent {
        bytes: Vec<u8>,
        rx_info: fidl_softmac::WlanRxInfo,
    },
}

/// Lifetime identity is captured before enqueue, never inferred from the
/// connection that happens to be current when this event is consumed.
pub(crate) struct Event {
    pub context: OperationContext,
    pub event: DriverEvent,
}

fn should_enable_minstrel(mac_sublayer: &fidl_common::MacSublayerSupport) -> bool {
    mac_sublayer
        .device
        .as_ref()
        .and_then(|device| device.tx_status_report_supported)
        .unwrap_or(false)
        && !mac_sublayer
            .rate_selection_offload
            .as_ref()
            .and_then(|selection| selection.supported)
            .unwrap_or(false)
}

const MINSTREL_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
// Remedy for https://fxbug.dev/42162128 (https://fxbug.dev/42108316)
// See |DATA_FRAME_INTERVAL_NANOS|
// in //src/connectivity/wlan/testing/hw-sim/test/rate_selection/src/lib.rs
// Ensure at least one probe frame (generated every 16 data frames)
// in every cycle:
// 16 <= (MINSTREL_UPDATE_INTERVAL_HW_SIM / MINSTREL_DATA_FRAME_INTERVAL_NANOS * 1e6) < 32.
const MINSTREL_UPDATE_INTERVAL_HW_SIM: std::time::Duration = std::time::Duration::from_millis(83);

// Preserve the upstream serving boundary with explicit native endpoints.
#[expect(clippy::too_many_arguments)]
pub(crate) async fn mlme_main_loop<T: MlmeImpl>(
    init_sender: oneshot::Sender<()>,
    config: T::Config,
    mut device: T::Device,
    mlme_request_stream: mpsc::Receiver<Request>,
    driver_event_stream: mpsc::Receiver<Event>,
    execution: Rc<MlmeExecution>,
    deadline: Arc<Mutex<Option<Instant>>>,
    pending: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
) -> Result<(), Error>
where
    T::TimerEvent: Send + 'static,
{
    info!("Starting MLME main loop...");
    let (minstrel_timer, minstrel_time_stream) = common::timer::create_timer();
    let minstrel = device
        .mac_sublayer_support()
        .await
        .ok()
        .filter(should_enable_minstrel)
        .map(|mac_sublayer_support| {
            let minstrel = wlan_mlme::new_minstrel(
                minstrel_timer,
                if mac_sublayer_support
                    .device
                    .and_then(|device| device.is_synthetic)
                    .unwrap_or(false)
                {
                    MINSTREL_UPDATE_INTERVAL_HW_SIM
                } else {
                    MINSTREL_UPDATE_INTERVAL
                },
            );
            device.set_minstrel(minstrel.clone());
            minstrel
        });

    let (timer_sender, time_stream) = mpsc::channel(256);
    let timer_sender = Mutex::new(timer_sender);
    let origin = execution.operation.clone();
    let timer_overflow = overflow.clone();
    let timer = common::timer::Timer::new(UnboundedSink::native(
        move |(at, event, handle): common::timer::ScheduledEvent<T::TimerEvent>| {
            let epoch = origin.lock().unwrap().epoch().clone();
            if timer_sender
                .lock()
                .unwrap()
                .try_send((
                    at,
                    common::timer::Event {
                        id: event.id,
                        event: (epoch, event.event),
                    },
                    handle,
                ))
                .is_err()
            {
                timer_overflow.store(true, Ordering::Release);
            }
        },
    ));

    // Native construction returns an error to supervision instead of panicking;
    // only the hardware owner can subsequently certify containment.
    let mlme_impl = T::new(config, device, timer).await?;
    init_sender
        .send(())
        .map_err(|_| format_err!("Failed to signal init complete."))?;

    main_loop_impl(
        mlme_impl,
        minstrel,
        mlme_request_stream,
        driver_event_stream,
        time_stream,
        minstrel_time_stream,
        execution,
        deadline,
        pending,
        overflow,
    )
    .await
}

/// Runs until explicit protocol stop or a terminal serving failure.
/// A successful return says nothing about DMA ownership.
// Preserve the upstream serving boundary with explicit native endpoints.
#[expect(clippy::too_many_arguments)]
async fn main_loop_impl<T: MlmeImpl>(
    mut mlme_impl: T,
    minstrel: Option<MinstrelWrapper>,
    mut mlme_request_stream: mpsc::Receiver<Request>,
    mut driver_event_stream: mpsc::Receiver<Event>,
    time_stream: mpsc::Receiver<common::timer::ScheduledEvent<(OperationEpoch, T::TimerEvent)>>,
    minstrel_time_stream: common::timer::EventStream<()>,
    execution: Rc<MlmeExecution>,
    deadline: Arc<Mutex<Option<Instant>>>,
    pending: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
) -> Result<(), Error> {
    let mut timer_stream = common::timer::make_async_timed_event_stream(time_stream).fuse();
    let mut minstrel_timer_stream =
        common::timer::make_async_timed_event_stream(minstrel_time_stream).fuse();

    loop {
        if overflow.load(Ordering::Acquire) {
            bail!("MLME native queue overflow");
        }
        select! {
            mlme_request = mlme_request_stream.next() => match mlme_request {
                Some(Request { context, scan, request }) => {
                    if context.is_live() {
                        execution.epoch.replace(context.epoch().clone());
                        *execution.operation.lock().unwrap() = context;
                        execution.rejected.set(false);
                        if let wlan_sme::MlmeRequest::Scan(request) = &request {
                            let context = scan.ok_or_else(|| format_err!("Scan has no authority"))?;
                            if execution.scan.borrow().is_some() {
                                bail!("Scan already in progress");
                            }
                            execution.scan.replace(Some(ScanOperation {
                                transaction_id: request.txn_id, device_scan_id: None, context,
                            }));
                        }
                        let method_name = request.name();
                        if let Err(error) = mlme_impl.handle_mlme_request(request).await {
                            // As upstream, a rejected protocol request is not a
                            // process failure. Fatal device errors have a separate
                            // supervised owner channel and are never swallowed here.
                            info!("Failed to handle mlme {} request: {}", method_name, error);
                        }
                    }
                    pending.fetch_sub(1, Ordering::AcqRel);
                }
                None => bail!("MLME request stream terminated unexpectedly."),
            },
            driver_event = driver_event_stream.next() => match driver_event {
                Some(Event { context, event }) => {
                    if context.is_live() || matches!(&event,
                        DriverEvent::Stop { .. } | DriverEvent::ScanComplete { .. })
                    {
                        execution.epoch.replace(context.epoch().clone());
                        *execution.operation.lock().unwrap() = context;
                        execution.rejected.set(false);
                        match event {
                            DriverEvent::Stop { responder } => {
                                responder.send(()).map_err(|_| format_err!("Stop receiver closed"))?;
                                pending.fetch_sub(1, Ordering::AcqRel);
                                return Ok(());
                            }
                            DriverEvent::ScanComplete { status, scan_id } => {
                                mlme_impl.handle_scan_complete(status, scan_id).await;
                            }
                            DriverEvent::TxResultReport { tx_result } => {
                                if let Some(minstrel) = minstrel.as_ref() {
                                    minstrel.lock().handle_tx_result_report(&tx_result);
                                }
                            }
                            DriverEvent::EthernetTxEvent(bytes) => {
                                if let Err(error) = mlme_impl.handle_eth_frame_tx(
                                    &bytes, fuchsia_trace::Id::new(),
                                ) {
                                    info!("Failed to handle eth frame: {}", error);
                                }
                            }
                            DriverEvent::WlanRxEvent { bytes, rx_info } => {
                                mlme_impl.handle_mac_frame_rx(
                                    &bytes, rx_info, fuchsia_trace::Id::new(),
                                ).await;
                            }
                        }
                    }
                    pending.fetch_sub(1, Ordering::AcqRel);
                }
                None => bail!("Driver event stream terminated unexpectedly."),
            },
            timed_event = timer_stream.select_next_some() => {
                let (epoch, event) = timed_event.event;
                if epoch.is_live() {
                    // A live association may issue new bounded work after its
                    // connect budget ends. Queued requests never renew budgets.
                    let end = deadline.lock().unwrap().unwrap_or_else(
                        || Instant::now() + Duration::from_secs(3),
                    );
                    *execution.operation.lock().unwrap() = epoch.context(end);
                    execution.epoch.replace(epoch);
                    execution.rejected.set(false);
                    pending.fetch_add(1, Ordering::AcqRel);
                    mlme_impl.handle_timeout(event).await;
                    pending.fetch_sub(1, Ordering::AcqRel);
                }
            },
            _minstrel_timeout = minstrel_timer_stream.select_next_some() => {
                if let Some(minstrel) = minstrel.as_ref() {
                    minstrel.lock().handle_timeout();
                }
            }
        }
    }
}
