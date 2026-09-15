// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Adapted from Fuchsia 1e1219e3fac944c9a906aea9646939746b6062b3:
// src/connectivity/wlan/lib/sme/src/serve/client.rs.
// Retain capability-derived configuration and independent request/event serving.
// Native replies acknowledge protocol admission, never hardware cleanup.

use crate::{OperationContext, OperationEpoch, mlme};
use anyhow::format_err;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use futures::channel::{mpsc, oneshot};
use futures::{Future, FutureExt, SinkExt, StreamExt, select};
use std::cell::RefCell;
use std::pin::pin;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Instant;
use wlan_common::{sink::UnboundedSink, timer};
use wlan_sme::client::{ClientSme, ConnectTransactionStream};

pub(crate) type ScanReceiver =
    oneshot::Receiver<Result<Vec<wlan_common::scan::ScanResult>, fidl_mlme::ScanResultCode>>;

pub(crate) type ConnectTransaction = mpsc::Receiver<fidl_sme::ConnectTransactionEvent>;

pub(crate) enum Request {
    Connect {
        context: OperationContext,
        request: fidl_sme::ConnectRequest,
        reply: oneshot::Sender<ConnectTransaction>,
    },
    Scan {
        context: OperationContext,
        scan: OperationContext,
        request: fidl_sme::ScanRequest,
        reply: oneshot::Sender<ScanReceiver>,
    },
    Disconnect {
        context: OperationContext,
        reason: fidl_sme::UserDisconnectReason,
        reply: oneshot::Sender<()>,
    },
}

pub(crate) fn serve(
    mut cfg: wlan_sme::client::ClientConfig,
    device_info: fidl_mlme::DeviceInfo,
    security_support: fidl_common::SecuritySupport,
    spectrum_management_support: fidl_common::SpectrumManagementSupport,
    event_stream: mpsc::Receiver<(OperationEpoch, fidl_mlme::MlmeEvent)>,
    mut requests: mpsc::Receiver<Request>,
    mlme_requests: mpsc::Sender<mlme::Request>,
    inspector: fuchsia_inspect::Inspector,
    initial_context: OperationContext,
    deadline: Arc<Mutex<Option<Instant>>>,
    pending: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
) -> (
    Rc<RefCell<ClientSme>>,
    impl Future<Output = Result<(), anyhow::Error>>,
) {
    // These are derived exactly as in upstream client::serve, not independently
    // supplied by each chip entrypoint.
    cfg.wpa3_supported = security_support
        .mfp
        .as_ref()
        .is_some_and(|mfp| mfp.supported.unwrap_or(false))
        && security_support.sae.as_ref().is_some_and(|sae| {
            sae.driver_handler_supported.unwrap_or(false)
                || sae.sme_handler_supported.unwrap_or(false)
        });
    cfg.owe_supported = security_support
        .mfp
        .as_ref()
        .is_some_and(|mfp| mfp.supported.unwrap_or(false))
        && security_support
            .owe
            .as_ref()
            .is_some_and(|owe| owe.supported.unwrap_or(false));

    let origin = Arc::new(Mutex::new(initial_context));
    let scan_origin = Arc::new(Mutex::new(None::<OperationContext>));
    let request_origin = origin.clone();
    let request_scan = scan_origin.clone();
    let request_overflow = overflow.clone();
    let mlme_requests = Mutex::new(mlme_requests);
    let mlme_sink = UnboundedSink::native(move |request| {
        let context = request_origin.lock().unwrap().clone();
        let scan = if matches!(&request, wlan_sme::MlmeRequest::Scan(_)) {
            request_scan.lock().unwrap().clone()
        } else {
            None
        };
        pending.fetch_add(1, Ordering::AcqRel);
        if mlme_requests
            .lock()
            .unwrap()
            .try_send(mlme::Request {
                context,
                scan,
                request,
            })
            .is_err()
        {
            pending.fetch_sub(1, Ordering::AcqRel);
            request_overflow.store(true, Ordering::Release);
        }
    });

    let (timer_sender, time_stream) = mpsc::channel(256);
    let timer_sender = Mutex::new(timer_sender);
    let timer_origin = origin.clone();
    let timer_overflow = overflow.clone();
    let timer = timer::Timer::new(UnboundedSink::native(
        move |(at, event, handle): timer::ScheduledEvent<
            <ClientSme as wlan_sme::Station>::Event,
        >| {
            let epoch = timer_origin.lock().unwrap().epoch().clone();
            if timer_sender
                .lock()
                .unwrap()
                .try_send((
                    at,
                    timer::Event {
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

    let node = inspector.root().create_child("sme");
    let sme = Rc::new(RefCell::new(ClientSme::new_with_sinks(
        cfg,
        device_info,
        inspector,
        node,
        security_support,
        spectrum_management_support,
        mlme_sink,
        timer,
    )));
    let station = sme.clone();
    let fut = async move {
        let mlme_sme = super::serve_mlme_sme(
            event_stream,
            station.clone(),
            time_stream,
            origin.clone(),
            deadline,
            overflow,
        );
        let sme_requests = async move {
            let mut transactions = futures::stream::FuturesUnordered::new();
            loop {
                select! {
                    result = transactions.select_next_some() => {
                        if let Err(error) = result {
                            log::info!("Connect transaction ended: {error}");
                        }
                        continue;
                    },
                    request = requests.next() => {
                        let Some(request) = request else {
                            return Err::<(), _>(format_err!("SME request stream ended unexpectedly"));
                        };
                        match request {
                    Request::Connect {
                        context,
                        request,
                        reply,
                    } => {
                        if !context.is_live() {
                            continue;
                        }
                        *origin.lock().unwrap() = context;
                        let transaction = station.borrow_mut().on_connect_command(request);
                        let (events, receiver) = mpsc::channel(64);
                        let _ = reply.send(receiver);
                        transactions.push(serve_connect_txn_stream(Some(events), transaction));
                    }
                    Request::Scan {
                        context,
                        scan,
                        request,
                        reply,
                    } => {
                        if !context.is_live() {
                            continue;
                        }
                        *origin.lock().unwrap() = context;
                        *scan_origin.lock().unwrap() = Some(scan);
                        let result = station.borrow_mut().on_scan_command(request);
                        let _ = reply.send(result);
                    }
                    Request::Disconnect {
                        context,
                        reason,
                        reply,
                    } => {
                        if !context.is_live() {
                            continue;
                        }
                        *origin.lock().unwrap() = context;
                        station
                            .borrow_mut()
                            .on_disconnect_command(reason, Default::default());
                        let _ = reply.send(());
                    }
                        }
                    }
                }
            }
        };
        let mlme_sme = pin!(mlme_sme);
        let sme_requests = pin!(sme_requests);
        select! {
            result = mlme_sme.fuse() => result,
            result = sme_requests.fuse() => result,
        }
    };
    (sme, fut)
}

// As in upstream serve_connect_txn_stream, SME owns the connection lifetime;
// the server forwards its results, reconnect, signal and channel events.
async fn serve_connect_txn_stream(
    handle: Option<mpsc::Sender<fidl_sme::ConnectTransactionEvent>>,
    mut connect_txn_stream: ConnectTransactionStream,
) -> Result<(), anyhow::Error> {
    if let Some(mut handle) = handle {
        while let Some(event) = connect_txn_stream.next().await {
            handle.send(event.into_fidl()).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_transaction_forwards_terminal_result_and_closes_with_sme() {
        futures::executor::block_on(async {
            let (source, stream) = mpsc::unbounded();
            let (target, mut receiver) = mpsc::channel(1);
            source
                .unbounded_send(wlan_sme::client::ConnectTransactionEvent::OnConnectResult {
                    result: wlan_sme::client::ConnectResult::Canceled,
                    is_reconnect: false,
                })
                .unwrap();
            drop(source);
            let (result, event) = futures::join!(
                serve_connect_txn_stream(Some(target), stream),
                receiver.next(),
            );
            result.unwrap();
            let Some(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) = event else {
                panic!("missing connect result");
            };
            assert_eq!(
                result.code,
                fidl_fuchsia_wlan_ieee80211::StatusCode::Canceled
            );
            assert!(!result.is_reconnect);
            assert!(receiver.next().await.is_none());
        });
    }
}
