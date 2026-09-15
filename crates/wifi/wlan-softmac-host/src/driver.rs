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
        OperationContext,
        WlanKeyConfiguration,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Association(
        OperationContext,
        WlanAssociationConfig,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    ClearAssociation(
        OperationContext,
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
            | Self::Key(context, ..)
            | Self::Association(context, ..)
            | Self::ClearAssociation(context, ..)
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
            Self::Key(_, _, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::Association(_, _, reply) => {
                let _ = reply.send(Err(status));
            }
            Self::ClearAssociation(_, _, reply) => {
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
    waker: Option<std::task::Waker>,
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
        if let Some(waker) = mailbox.waker.take() {
            waker.wake();
        }
        Ok(())
    }
}

/// Native lifecycle operations are distinct from protocol requests. A stop or
/// reset can revoke a suspended request without dropping its DMA ownership.
pub(crate) enum OwnerCommand {
    Link(bool, oneshot::Sender<Result<(), zx::Status>>),
    FinishAttempt(oneshot::Sender<Result<(), zx::Status>>),
    PowerSave(
        OperationContext,
        bool,
        oneshot::Sender<Result<(), zx::Status>>,
    ),
    Stop,
    Reset,
}

pub(crate) struct DriverActor<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> {
    device: D,
    mailbox: Rc<RefCell<Mailbox>>,
    pending: Option<Pin<Box<dyn Future<Output = ()>>>>,
    stop_pending: bool,
    reset_completed: bool,
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> DriverActor<D> {
    pub(crate) fn new(device: D) -> (Self, DriverHandle) {
        let mailbox = Rc::new(RefCell::new(Mailbox {
            queue: VecDeque::new(),
            closed: false,
            waker: None,
        }));
        let handle = DriverHandle(mailbox.clone());
        (
            Self {
                device,
                mailbox,
                pending: None,
                stop_pending: false,
                reset_completed: false,
            },
            handle,
        )
    }

    /// Independently progress hardware while MLME awaits DeviceOps. The device
    /// never leaves this owner until the loop returns it for final containment.
    pub(crate) async fn serve(
        mut self,
        mut control: futures::channel::mpsc::Receiver<OwnerCommand>,
    ) -> (Self, Result<(), zx::Status>) {
        use futures::{FutureExt, StreamExt};
        let mut timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
        loop {
            // Recompute after every admitted command. Preserve an earlier armed
            // observation so unrelated wakes cannot slide the fallback timer.
            match self.device.next_deadline() {
                None => timer = None,
                Some(deadline) => {
                    let deadline = tokio::time::Instant::from_std(deadline);
                    if timer
                        .as_ref()
                        .is_none_or(|timer| deadline < timer.deadline())
                    {
                        timer = Some(Box::pin(tokio::time::sleep_until(deadline)));
                    }
                }
            }
            let mut timer_fired = false;
            let turn = {
                let command = control.next().fuse();
                let tick = std::future::poll_fn(|cx| match timer.as_mut() {
                    Some(timer) => timer.as_mut().poll(cx),
                    None => Poll::Pending,
                })
                .fuse();
                let work = std::future::poll_fn(|cx| match self.poll(cx) {
                    Ok(true) => Poll::Ready(Ok(())),
                    Ok(false) => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                })
                .fuse();
                futures::pin_mut!(command, tick, work);
                futures::select_biased! {
                    command = command => Ok(Some(command)),
                    result = work => result.map(|()| None),
                    _ = tick => { timer_fired = true; Ok(None) },
                }
            };
            if timer_fired {
                timer = None;
            }
            match turn {
                Err(error) => {
                    self.close();
                    return (self, Err(error));
                }
                Ok(Some(Some(OwnerCommand::Link(up, reply)))) => {
                    let _ = reply.send(self.set_link_up(up));
                }
                Ok(Some(Some(OwnerCommand::FinishAttempt(reply)))) => {
                    let _ = reply.send(self.finish_failed_connect_attempt());
                }
                Ok(Some(Some(OwnerCommand::PowerSave(context, enabled, reply)))) => {
                    if self.pending.is_some() {
                        let _ = reply.send(Err(zx::Status::SHOULD_WAIT));
                    } else if let Err(status) = context.check(std::time::Instant::now()) {
                        let _ = reply.send(Err(status));
                    } else {
                        let completion = self.device.set_power_save_mode(context, enabled);
                        self.pending = Some(Box::pin(async move {
                            let _ = reply.send(completion.await);
                        }));
                    }
                }
                Ok(Some(Some(OwnerCommand::Reset))) => {
                    let result = self.reset();
                    return (self, result);
                }
                Ok(Some(Some(OwnerCommand::Stop))) | Ok(Some(None)) => {
                    let result = self.stop();
                    return (self, result);
                }
                Ok(None) => {}
            }
            tokio::task::yield_now().await;
        }
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
        if !self.reset_completed {
            self.device.reset()?;
            self.reset_completed = true;
            self.stop_pending = false;
        }
        Ok(())
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

    #[cfg(test)]
    pub(crate) async fn drive_once(&mut self) -> Result<bool, zx::Status> {
        std::future::poll_fn(|cx| Poll::Ready(self.poll(cx))).await
    }

    /// Drive a device-boundary fixture without a second mutable hardware reference.
    #[cfg(test)]
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
            self.device.poll_drive(cx)?
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
            self.mailbox.borrow_mut().waker = Some(cx.waker().clone());
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
            Command::Key(context, request, reply) => {
                let completion = self.device.install_key(context, request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::Association(context, request, reply) => {
                let completion = self.device.notify_association_complete(context, request);
                self.pending = Some(Box::pin(async move {
                    let _ = reply.send(completion.await);
                }));
            }
            Command::ClearAssociation(context, request, reply) => {
                let completion = self.device.clear_association(context, request);
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
                match self.device.queue_tx(context.clone(), &bytes, flags) {
                    Err(zx::Status::NO_RESOURCES) => {
                        // Admission did not publish the frame. Retain it within
                        // the existing mailbox bound while hardware drains;
                        // retry with the same authority and original deadline.
                        self.mailbox.borrow_mut().queue.push_front(Message {
                            epoch: message.epoch,
                            command: Command::Transmit(context, bytes, flags),
                        });
                        return Ok(progressed);
                    }
                    result => result?,
                }
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

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> Drop for DriverActor<D> {
    fn drop(&mut self) {
        // Task cancellation and discarded task output must not bypass the
        // owner's final stop attempt. Device resource RAII remains the backstop;
        // Drop is not a cleanup certificate.
        let _ = self.stop();
    }
}

/// Ownership survives canceled shutdown waiters and failed containment attempts.
/// The protocol side receives only command capabilities, never shared mutable D.
pub(crate) enum HardwareOwner<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> {
    Running {
        task: tokio::task::JoinHandle<(DriverActor<D>, Result<(), zx::Status>)>,
        control: futures::channel::mpsc::Sender<OwnerCommand>,
    },
    Returned {
        actor: DriverActor<D>,
        result: Result<(), zx::Status>,
    },
    Lost,
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> HardwareOwner<D> {
    pub(crate) fn observe(&mut self) -> Option<Result<(), zx::Status>> {
        use futures::FutureExt;
        if let Self::Running { task, .. } = self {
            match task.now_or_never()? {
                Ok((actor, result)) => *self = Self::Returned { actor, result },
                Err(_) => *self = Self::Lost,
            }
        }
        Some(match self {
            Self::Returned { result, .. } => *result,
            Self::Lost => Err(zx::Status::IO),
            Self::Running { .. } => unreachable!(),
        })
    }

    pub(crate) fn send(&mut self, command: OwnerCommand) -> Result<(), zx::Status> {
        match self {
            Self::Running { control, .. } => control.try_send(command).map_err(|error| {
                if error.is_full() {
                    zx::Status::NO_RESOURCES
                } else {
                    zx::Status::BAD_STATE
                }
            }),
            _ => Err(zx::Status::BAD_STATE),
        }
    }

    /// Admission only. The caller retains reset escalation even if closing the
    /// full control channel can initially request only a stop.
    pub(crate) fn request_stop(&mut self, reset: bool) {
        if let Self::Running { control, .. } = self {
            let command = if reset {
                OwnerCommand::Reset
            } else {
                OwnerCommand::Stop
            };
            if control.try_send(command).is_err() {
                control.close_channel();
            }
        }
    }

    pub(crate) async fn join(&mut self) {
        if let Self::Running { task, .. } = self {
            // Borrow, do not take: dropping this waiter cannot detach the task
            // or discard the actor it will return.
            let result = task.await;
            *self = match result {
                Ok((actor, result)) => Self::Returned { actor, result },
                Err(_) => Self::Lost,
            };
        }
    }

    pub(crate) fn certify(&mut self, reset: bool) -> Result<(), zx::Status> {
        match self {
            Self::Returned { actor, result } => {
                *result = if reset { actor.reset() } else { actor.stop() };
                *result
            }
            Self::Running { .. } => Err(zx::Status::SHOULD_WAIT),
            Self::Lost => Err(zx::Status::IO),
        }
    }
}

impl<D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver> Drop for HardwareOwner<D> {
    fn drop(&mut self) {
        if let Self::Running { task, control } = self {
            control.close_channel();
            task.abort();
        }
    }
}
