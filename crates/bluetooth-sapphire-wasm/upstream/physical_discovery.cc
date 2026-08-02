// Bounded Sapphire LE discovery reactor for pinned Pigweed.

#include <stdint.h>
#include <string.h>

#include <memory>
#include <unordered_set>

#include "pw_async/fake_dispatcher.h"
#include "pw_bluetooth/controller.h"
#include "pw_bluetooth_sapphire/null_lease_provider.h"
#include "pw_bluetooth_sapphire/internal/host/hci/fake_local_address_delegate.h"
#include "pw_bluetooth_sapphire/internal/host/hci/legacy_low_energy_scanner.h"
#include "pw_bluetooth_sapphire/internal/host/transport/transport.h"

extern "C" __attribute__((import_module(
    "drv:bluetooth-sapphire/controller@0.1.0"), import_name("send"))) uint32_t
drv_controller_send(uint32_t, const uint8_t*, uint32_t);

namespace {
constexpr uint32_t kInitializing = 1;
constexpr uint32_t kScanning = 2;
constexpr uint32_t kStopping = 3;
constexpr uint32_t kStopped = 4;
constexpr uint32_t kFailed = 5;
constexpr uint16_t kReset = 0x0c03;
constexpr uint16_t kSetEventMask = 0x0c01;
constexpr uint16_t kLeSetEventMask = 0x2001;
alignas(16) uint8_t rx_buffer[4096];

bool SendRaw(uint16_t opcode, const uint8_t* params, uint8_t length) {
  uint8_t packet[11] = {static_cast<uint8_t>(opcode),
                        static_cast<uint8_t>(opcode >> 8), length};
  memcpy(packet + 3, params, length);
  return drv_controller_send(0, packet, 3 + length) == 0;
}

class IpcController final : public pw::bluetooth::Controller {
 public:
  void SetEventFunction(DataFunction f) override { event_ = std::move(f); }
  void SetReceiveAclFunction(DataFunction f) override { acl_ = std::move(f); }
  void SetReceiveScoFunction(DataFunction f) override { sco_ = std::move(f); }
  void SetReceiveIsoFunction(DataFunction f) override { iso_ = std::move(f); }
  void Initialize(pw::Callback<void(pw::Status)> cb,
                  pw::Callback<void(pw::Status)> error) override {
    error_ = std::move(error);
    cb(pw::OkStatus());
  }
  void Close(pw::Callback<void(pw::Status)> cb) override {
    event_ = nullptr;
    acl_ = nullptr;
    sco_ = nullptr;
    iso_ = nullptr;
    cb(pw::OkStatus());
  }
  void SendCommand(pw::span<const std::byte> p) override { Send(0, p); }
  void SendAclData(pw::span<const std::byte>) override { Fail(); }
  void SendScoData(pw::span<const std::byte>) override { Fail(); }
  void SendIsoData(pw::span<const std::byte>) override { Fail(); }
  void ConfigureSco(ScoCodingFormat, ScoEncoding, ScoSampleRate,
                    pw::Callback<void(pw::Status)> cb) override {
    cb(pw::Status::Unimplemented());
  }
  void ResetSco(pw::Callback<void(pw::Status)> cb) override {
    cb(pw::Status::Unimplemented());
  }
  void GetFeatures(pw::Callback<void(FeaturesBits)> cb) override {
    cb(static_cast<FeaturesBits>(0));
  }
  void EncodeVendorCommand(
      pw::bluetooth::VendorCommandParameters,
      pw::Callback<void(pw::Result<pw::span<const std::byte>>)> cb) override {
    cb(pw::Status::Unimplemented());
  }
  bool Inject(uint32_t kind, const uint8_t* bytes, size_t length) {
    DataFunction* f = kind == 0 ? &event_ : kind == 1 ? &acl_ : kind == 2 ? &sco_ : &iso_;
    if (kind > 3 || !*f) return false;
    (*f)({reinterpret_cast<const std::byte*>(bytes), length});
    return true;
  }

 private:
  void Send(uint32_t kind, pw::span<const std::byte> p) {
    if (drv_controller_send(kind, reinterpret_cast<const uint8_t*>(p.data()), p.size())) Fail();
  }
  void Fail() {
    if (error_) error_(pw::Status::Internal());
  }
  DataFunction event_, acl_, sco_, iso_;
  pw::Callback<void(pw::Status)> error_;
};

class App final : public bt::hci::LowEnergyScanner::Delegate {
 public:
  bool Start() {
    state_ = kInitializing;
    stage_ = kReset;
    return SendRaw(kReset, nullptr, 0);
  }
  bool Inject(uint32_t kind, const uint8_t* bytes, size_t length) {
    if (state_ == kInitializing && stage_ != 0) return InitEvent(kind, bytes, length);
    if (!controller_ || !controller_->Inject(kind, bytes, length)) return false;
    dispatcher_.RunUntilIdle();
    return state_ != kFailed;
  }
  void Advance(uint32_t milliseconds) {
    if (milliseconds > elapsed_ms_) {
      dispatcher_.RunFor(std::chrono::milliseconds(milliseconds - elapsed_ms_));
      elapsed_ms_ = milliseconds;
    }
  }
  void Stop() {
    if (state_ == kScanning && scanner_ && scanner_->StopScan()) state_ = kStopping;
  }
  uint32_t state() const { return state_; }
  uint32_t peers() const { return peers_.size(); }
  void OnPeerFound(const std::unordered_set<uint16_t>&,
                   const bt::hci::LowEnergyScanResult& result) override {
    peers_.insert(result.address());
  }

 private:
  bool InitEvent(uint32_t kind, const uint8_t* b, size_t n) {
    if (kind != 0 || n < 6 || b[0] != 0x0e || b[1] < 4) return false;
    uint16_t opcode = static_cast<uint16_t>(
        static_cast<uint16_t>(b[3]) | (static_cast<uint16_t>(b[4]) << 8));
    if (opcode != stage_ || b[5] != 0) return false;
    if (stage_ == kReset) {
      static constexpr uint8_t mask[] = {0xff, 0xff, 0xfb, 0xff, 0x07, 0xf8, 0xbf, 0x3d};
      stage_ = kSetEventMask;
      return SendRaw(stage_, mask, sizeof(mask));
    }
    if (stage_ == kSetEventMask) {
      static constexpr uint8_t mask[] = {0x1f, 0, 0, 0, 0, 0, 0, 0};
      stage_ = kLeSetEventMask;
      return SendRaw(stage_, mask, sizeof(mask));
    }
    return BuildScanner();
  }
  bool BuildScanner() {
    stage_ = 0;
    auto controller = std::make_unique<IpcController>();
    controller_ = controller.get();
    transport_ = std::make_unique<bt::hci::Transport>(std::move(controller), dispatcher_, lease_);
    bool initialized = false;
    transport_->Initialize([&](bool ok) { initialized = ok; });
    if (!initialized) return false;
    scanner_ = std::make_unique<bt::hci::LegacyLowEnergyScanner>(
        &address_, bt::hci::AdvertisingPacketFilter::Config{
                       false, 0, bt::hci::AdvertisingPacketFilter::Config::DeliveryMode::kImmediate},
        transport_->GetWeakPtr(), dispatcher_);
    scanner_->SetPacketFilters(0, {});
    scanner_->set_delegate(this);
    bt::hci::LowEnergyScanner::ScanOptions options{.active = false,
                                                    .filter_duplicates = true,
                                                    .period = bt::hci::LowEnergyScanner::kPeriodInfinite};
    return scanner_->StartScan(options, [this](auto status) {
      using Status = bt::hci::LowEnergyScanner::ScanStatus;
      if (status == Status::kPassive) state_ = kScanning;
      else if (status == Status::kStopped) state_ = kStopped;
      else if (status == Status::kFailed) state_ = kFailed;
    });
  }
  pw::async::test::FakeDispatcher dispatcher_;
  pw::bluetooth_sapphire::NullLeaseProvider lease_;
  bt::hci::FakeLocalAddressDelegate address_{dispatcher_};
  IpcController* controller_ = nullptr;
  std::unique_ptr<bt::hci::Transport> transport_;
  std::unique_ptr<bt::hci::LegacyLowEnergyScanner> scanner_;
  std::unordered_set<bt::DeviceAddress> peers_;
  uint16_t stage_ = 0;
  uint32_t state_ = 0;
  uint32_t elapsed_ms_ = 0;
};
std::unique_ptr<App> app;
}  // namespace

extern "C" __attribute__((export_name("drv_discovery_start"))) int start() {
  app = std::make_unique<App>();
  return app->Start() ? 0 : 1;
}
extern "C" __attribute__((export_name("drv_rx_buffer"))) uintptr_t buffer() {
  return reinterpret_cast<uintptr_t>(rx_buffer);
}
extern "C" __attribute__((export_name("drv_rx_capacity"))) uint32_t capacity() { return sizeof(rx_buffer); }
extern "C" __attribute__((export_name("drv_inject_packet"))) int inject(uint32_t kind, uint32_t length) {
  return !app || length > sizeof(rx_buffer) || !app->Inject(kind, rx_buffer, length);
}
extern "C" __attribute__((export_name("drv_advance_time"))) void advance(uint32_t ms) { if (app) app->Advance(ms); }
extern "C" __attribute__((export_name("drv_request_stop"))) void stop() { if (app) app->Stop(); }
extern "C" __attribute__((export_name("drv_discovery_state"))) uint32_t state() { return app ? app->state() : kFailed; }
extern "C" __attribute__((export_name("drv_peer_count"))) uint32_t peers() { return app ? app->peers() : 0; }
int main(int, char**) { return 0; }
