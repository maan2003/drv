// SPDX-License-Identifier: GPL-2.0-only

//! Exclusive hardware owner and bounded in-process protocol mailbox.
//! Only this owner can invoke the device. A pending completion never borrows
//! the device and is retained independently of the protocol's reply receiver.

use crate::*;
use futures::channel::oneshot;
use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

const COMMAND_CAPACITY: usize = 256;

pub(crate) enum Command {
    Query(
        (),
        oneshot::Sender<Result<WlanSoftmacQueryResponse, zx::Status>>,
    ),
    Discovery((), oneshot::Sender<Result<DiscoverySupport, zx::Status>>),
    MacSublayer((), oneshot::Sender<Result<MacSublayerSupport, zx::Status>>),
    Security((), oneshot::Sender<Result<SecuritySupport, zx::Status>>),
    Spectrum(
        (),
        oneshot::Sender<Result<SpectrumManagementSupport, zx::Status>>,
    ),
    Channel(
        OperationContext,
        WlanSoftmacBaseSetChannelRequest,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Join(
        OperationContext,
        JoinBssRequest,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Key(
        WlanKeyConfiguration,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Association(
        WlanAssociationConfig,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    ClearAssociation(
        WlanSoftmacBaseClearAssociationRequest,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    PassiveScan(
        OperationContext,
        WlanSoftmacBaseStartPassiveScanRequest,
        oneshot::Sender<Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>>,
    ),
    ActiveScan(
        OperationContext,
        WlanSoftmacStartActiveScanRequest,
        oneshot::Sender<Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>>,
    ),
    CancelScan(
        WlanSoftmacBaseCancelScanRequest,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Wmm(
        WlanSoftmacBaseUpdateWmmParametersRequest,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Link(bool, oneshot::Sender<Result<(), zx::Status>>),
    Transmit(OperationContext, Vec<u8>, WlanTxInfoFlags),
}

impl Command {
    fn operation_context(&self) -> Option<&OperationContext> {
        match self {
            Self::Channel(context, ..)
            | Self::Join(context, ..)
            | Self::Transmit(context, ..)
            | Self::PassiveScan(context, ..)
            | Self::ActiveScan(context, ..) => Some(context),
            _ => None,
        }
    }

    fn reject(self, status: zx::Status) {
        match self {
            Self::Query(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Discovery(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::MacSublayer(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Security(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Spectrum(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Channel(_, _, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Join(_, _, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Key(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Association(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::ClearAssociation(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::PassiveScan(_, _, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::ActiveScan(_, _, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::CancelScan(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Wmm(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Link(_, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Transmit(..) => {}
        }
    }
}

struct Message {
    epoch: OperationEpoch,
    command: Command,
}

struct Mailbox {
    queue: VecDeque<Message>,
    closed: bool,
}

pub(crate) struct DriverHandle(Rc<RefCell<Mailbox>>);

impl DriverHandle {
    pub(crate) fn send(&self, epoch: OperationEpoch, command: Command) -> Result<(), zx::Status> {
        let mut mailbox = self.0.borrow_mut();
        if mailbox.closed {
            return Err(zx::Status::BAD_STATE);
        }
        if !epoch.is_live() {
            return Err(zx::Status::CANCELED);
        }
        if let Some(context) = command.operation_context() {
            context.check(std::time::Instant::now())?;
        }
        // Revocation discards only unpublished work. This also reserves space
        // for cleanup without a second priority queue or unbounded capacity.
        mailbox.queue.retain_mut(|message| {
            message.epoch.is_live()
                && message
                    .command
                    .operation_context()
                    .is_none_or(OperationContext::is_live)
        });
        if mailbox.queue.len() == COMMAND_CAPACITY {
            return Err(zx::Status::NO_RESOURCES);
        }
        mailbox.queue.push_back(Message { epoch, command });
        Ok(())
    }
}

pub(crate) struct DriverActor<D> {
    device: D,
    mailbox: Rc<RefCell<Mailbox>>,
    pending: Option<Pin<Box<dyn Future<Output = ()>>>>,
    stop_pending: bool,
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> DriverActor<D> {
    pub(crate) fn new(device: D) -> (Self, DriverHandle) {
        let mailbox = Rc::new(RefCell::new(Mailbox {
            queue: VecDeque::new(),
            closed: false,
        }));
        let handle = DriverHandle(mailbox.clone());
        (
            Self {
                device,
                mailbox,
                pending: None,
                stop_pending: false,
            },
            handle,
        )
    }

    pub(crate) fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        self.device.start(upcalls)?;
        self.stop_pending = true;
        Ok(())
    }

    pub(crate) fn stop(&mut self) -> Result<(), zx::Status> {
        self.close();
        if self.stop_pending {
            self.device.stop()?;
            self.stop_pending = false;
        }
        Ok(())
    }

    pub(crate) fn reset(&mut self) -> Result<(), zx::Status> {
        self.close();
        self.device.reset()
    }

    fn close(&mut self) {
        let mut mailbox = self.mailbox.borrow_mut();
        mailbox.closed = true;
        mailbox.queue.clear();
        // This is only a completion waiter. Hardware resources stay in device
        // until stop/reset or its final ownership teardown.
        self.pending = None;
    }

    pub(crate) fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        self.device.set_link_up(up)
    }

    pub(crate) fn finish_failed_connect_attempt(&mut self) -> Result<(), zx::Status> {
        if self.pending.is_some()
            || self
                .mailbox
                .borrow()
                .queue
                .iter()
                .any(|message| message.epoch.is_live())
        {
            return Err(zx::Status::SHOULD_WAIT);
        }
        self.device.finish_failed_connect_attempt()
    }

    pub(crate) async fn drive_once(&mut self) -> Result<bool, zx::Status> {
        std::future::poll_fn(|cx| Poll::Ready(self.poll(cx))).await
    }

    /// Drive constructor/test protocol work with the same owner, not a nested
    /// executor or a second mutable reference to the hardware.
    pub(crate) async fn run_until<F: Future>(
        &mut self,
        future: F,
    ) -> Result<F::Output, zx::Status> {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(|cx| {
            if let Poll::Ready(result) = future.as_mut().poll(cx) {
                return Poll::Ready(Ok(result));
            }
            match self.poll(cx) {
                Err(error) => Poll::Ready(Err(error)),
                Ok(progressed) => {
                    if progressed {
                        cx.waker().wake_by_ref();
                    }
                    Poll::Pending
                }
            }
        })
        .await
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Result<bool, zx::Status> {
        let mut progressed = if self.stop_pending {
            self.device.drive()?
        } else {
            false
        };
        if let Some(pending) = self.pending.as_mut() {
            if pending.as_mut().poll(cx).is_pending() {
                return Ok(progressed);
            }
            self.pending = None;
            progressed = true;
        }
        let message = self.mailbox.borrow_mut().queue.pop_front();
        let Some(message) = message else {
            return Ok(progressed);
        };
        if !message.epoch.is_live() {
            message.command.reject(zx::Status::CANCELED);
            return Ok(true);
        }
        if let Some(context) = message.command.operation_context()
            && let Err(status) = context.check(std::time::Instant::now())
        {
            message.command.reject(status);
            return Ok(true);
        }
        // No await between authority validation and the device call.
        match message.command {
            Command::Query((), reply) => {
                let _ = reply.send(self.device.query());
            }
            Command::Discovery((), reply) => {
                let _ = reply.send(self.device.query_discovery_support());
            }
            Command::MacSublayer((), reply) => {
                let _ = reply.send(self.device.query_mac_sublayer_support());
            }
            Command::Security((), reply) => {
                let _ = reply.send(self.device.query_security_support());
            }
            Command::Spectrum((), reply) => {
                let _ = reply.send(self.device.query_spectrum_management_support());
            }
            Command::Channel(context, request, reply) => {
                let completion = self.device.set_channel(context, request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::Join(context, request, reply) => {
                let completion = self.device.join_bss(context, request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::Key(request, reply) => {
                let completion = self.device.install_key(request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::Association(request, reply) => {
                let completion = self.device.notify_association_complete(request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::ClearAssociation(request, reply) => {
                let completion = self.device.clear_association(request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::PassiveScan(context, request, reply) => {
                let completion = self.device.start_passive_scan(context, request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::ActiveScan(context, request, reply) => {
                let completion = self.device.start_active_scan(context, request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::CancelScan(request, reply) => {
                let completion = self.device.cancel_scan(request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::Wmm(request, reply) => {
                let completion = self.device.update_wmm_parameters(request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::Link(request, reply) => {
                let _ = reply.send(self.device.set_link_up(request));
            }
            Command::Transmit(context, bytes, flags) => {
                self.device.queue_tx(context, &bytes, flags)?
            }
        }
        if let Some(pending) = self.pending.as_mut()
            && pending.as_mut().poll(cx).is_ready()
        {
            self.pending = None;
        }
        Ok(true)
    }
}
