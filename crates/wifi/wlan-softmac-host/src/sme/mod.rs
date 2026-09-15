// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Adapted from Fuchsia 1e1219e3fac944c9a906aea9646939746b6062b3:
// src/connectivity/wlan/lib/sme/src/serve/mod.rs.
// Native typed requests replace FIDL transport; SME still owns its event loop.

pub(crate) mod client;
use crate::{OperationContext, OperationEpoch};
use anyhow::format_err;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use futures::channel::mpsc;
use futures::{Stream, StreamExt, select};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use wlan_common::timer::{self, ScheduledEvent};
use wlan_sme::Station;

// The returned future successfully terminates when MLME closes the channel
async fn serve_mlme_sme<STA, TS>(
    mut event_stream: mpsc::Receiver<(OperationEpoch, fidl_mlme::MlmeEvent)>,
    station: Rc<RefCell<STA>>,
    time_stream: TS,
    origin: Arc<Mutex<OperationContext>>,
    deadline: Arc<Mutex<Option<Instant>>>,
    overflow: Arc<AtomicBool>,
) -> Result<(), anyhow::Error>
where
    STA: Station,
    TS: Stream<Item = ScheduledEvent<(OperationEpoch, <STA as Station>::Event)>> + Unpin,
{
    let mut timeout_stream = timer::make_async_timed_event_stream(time_stream).fuse();

    loop {
        if overflow.load(Ordering::Acquire) {
            return Err(format_err!("SME native queue overflow"));
        }
        select! {
            // Fuse rationale: any `none`s in the MLME stream should result in
            // bailing immediately, so we don't need to track if we've seen a
            // `None` or not and can `fuse` directly in the `select` call.
            mlme_event = event_stream.next() => match mlme_event {
                Some((epoch, mlme_event)) => {
                    if epoch.is_live() {
                        let end = deadline.lock().unwrap().unwrap_or_else(
                            || Instant::now() + Duration::from_secs(3),
                        );
                        *origin.lock().unwrap() = epoch.context(end);
                        station.borrow_mut().on_mlme_event(mlme_event);
                    }
                },
                None => return Ok(()),
            },
            timeout = timeout_stream.next() => match timeout {
                Some(timed_event) => {
                    let (epoch, event) = timed_event.event;
                    if epoch.is_live() {
                        let end = deadline.lock().unwrap().unwrap_or_else(
                            || Instant::now() + Duration::from_secs(3),
                        );
                        *origin.lock().unwrap() = epoch.context(end);
                        station.borrow_mut().on_timeout(timer::Event { id: timed_event.id, event });
                    }
                },
                None => return Err(format_err!("SME timer stream has ended unexpectedly")),
            },
        }
    }
}
