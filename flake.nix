{
  description = "Development environment for drv";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "aarch64-linux"
        "x86_64-linux"
      ];
    in
    {
      nixosModules.netstack3-kernel-provider = import ./crates/net/netstack3-port-spike/kernel-provider/module.nix;

      checks.x86_64-linux.vfio-edu =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./nix/vfio-edu-test.nix
          { };

      checks.x86_64-linux.netstack3-kernel-provider =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/net/netstack3-port-spike/kernel-provider/check.nix
          { };

      checks.x86_64-linux.audio-pipewire-daemon =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./lab/audio/audio-pipewire-spike/package.nix
          { };

      checks.x86_64-linux.netstack3-provider-daemon =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./crates/net/netstack3-port-spike/provider-package.nix
          { };

      checks.x86_64-linux.netstack3-provider-service =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/net/netstack3-port-spike/kernel-provider/service-test.nix
          { };

      checks.x86_64-linux.netstack3-kernel-provider-boot =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/net/netstack3-port-spike/kernel-provider/boot-test.nix
          { };

      checks.x86_64-linux.wlan-softmac-host =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "wlan-softmac-host-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/wlan-softmac-host";
          buildAndTestSubdir = "crates/wifi/wlan-softmac-host";
          cargoLock.lockFile = ./crates/wifi/wlan-softmac-host/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.pkg-config pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/wlan-softmac-host
            cargo test --locked --offline
            cargo clippy --locked --offline --all-targets -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.wlan-control-wire =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "wlan-control-wire-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/wlan-control-wire";
          buildAndTestSubdir = "crates/wifi/wlan-control-wire";
          cargoLock.lockFile = ./crates/wifi/wlan-control-wire/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/wlan-control-wire
            cargo test --locked --offline
            cargo clippy --locked --offline --all-targets -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            mkdir -p "$out/share/wlan-control-wire"
            cp Cargo.toml "$out/share/wlan-control-wire/"
          '';
        };

      checks.x86_64-linux.linux-self-sandbox =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "linux-self-sandbox-check";
          version = "0.1.0";
          src = ./.;
          cargoRoot = "crates/platform/linux-self-sandbox";
          buildAndTestSubdir = "crates/platform/linux-self-sandbox";
          cargoLock.lockFile = ./crates/platform/linux-self-sandbox/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/platform/linux-self-sandbox
            cargo test --locked --offline -- --nocapture
            cargo test --locked --offline --features filter-integration-test \
              --test mt_hashmap_filter -- --nocapture
            cargo clippy --locked --offline --all-targets --all-features -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.wifi-control-service =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "wifi-control-service-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/wifi-control-service";
          buildAndTestSubdir = "crates/wifi/wifi-control-service";
          cargoLock.lockFile = ./crates/wifi/wifi-control-service/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/wifi-control-service
            cargo test --locked --offline --features test-fixture
            cargo clippy --locked --offline --all-targets --features test-fixture -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.ath11k-wifi-service =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "ath11k-wifi-service-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/drivers/ath11k/ath11k-wifi-service";
          buildAndTestSubdir = "crates/wifi/drivers/ath11k/ath11k-wifi-service";
          cargoLock.lockFile = ./crates/wifi/drivers/ath11k/ath11k-wifi-service/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/drivers/ath11k/ath11k-wifi-service
            cargo test --locked --offline
            cargo clippy --locked --offline --all-targets -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.wlancfg-service =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "wlancfg-service-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/wlancfg-service";
          buildAndTestSubdir = "crates/wifi/wlancfg-service";
          cargoLock.lockFile = ./crates/wifi/wlancfg-service/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/wlancfg-service
            cargo test --locked --offline
            # This filtered lifecycle must match deployment. Rust's debug-only
            # I/O-safety assertions use fcntl, which the runtime policy
            # intentionally kills rather than admitting for test convenience.
            cargo test --locked --offline --release \
              --features filter-integration-test --test filtered-control-owner
            cargo clippy --locked --offline --all-targets --all-features -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.network-service =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "network-service-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/net/network-service";
          buildAndTestSubdir = "crates/net/network-service";
          cargoLock.lockFile = ./crates/net/network-service/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.pkg-config pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/net/network-service
            cargo test --locked --offline
            cargo clippy --locked --offline --all-targets -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.ath11k-softmac-adapter =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "ath11k-softmac-adapter-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/drivers/ath11k/ath11k-softmac-adapter";
          buildAndTestSubdir = "crates/wifi/drivers/ath11k/ath11k-softmac-adapter";
          cargoLock.lockFile = ./crates/wifi/drivers/ath11k/ath11k-softmac-adapter/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.pkg-config pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/drivers/ath11k/ath11k-softmac-adapter
            cargo test --locked --offline
            cargo clippy --locked --offline --all-targets -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.mt7921-production-client =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "mt7921-production-client-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "crates/wifi/drivers/mt7921/mt7921-production-client";
          buildAndTestSubdir = "crates/wifi/drivers/mt7921/mt7921-production-client";
          cargoLock.lockFile = ./crates/wifi/drivers/mt7921/mt7921-production-client/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.pkg-config pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd crates/wifi/drivers/mt7921/mt7921-production-client
            cargo test --locked --offline
            cargo clippy --locked --offline --all-targets --no-deps -- -D warnings
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      checks.x86_64-linux.wlancfg-saved-networks =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
          commit = mt7921FuchsiaSource.fuchsiaBaseRevision;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "wlancfg-saved-networks-check";
          version = "0.1.0";
          src = mt7921FuchsiaSource;
          cargoRoot = "reference/fuchsia-${commit}/src/connectivity/network/netstack3";
          buildAndTestSubdir = "reference/fuchsia-${commit}/src/connectivity/network/netstack3";
          cargoLock.lockFile = ./crates/net/netstack3-port-spike/upstream-cargo/src/connectivity/network/netstack3/Cargo.lock;
          nativeBuildInputs = [ pkgs.clippy pkgs.cmake pkgs.pkg-config pkgs.perl ];
          dontBuild = true;
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cd reference/fuchsia-${commit}/src/connectivity/network/netstack3
            cargo test --locked --offline -p directory-capability
            cargo clippy --locked --offline -p directory-capability --all-targets -- -D warnings
            cargo test --locked --offline -p wlancfg-selection --test host-saved-networks
            cargo test --locked --offline -p wlancfg-selection --test host-selection
            runHook postCheck
          '';
          installPhase = ''
            touch "$out"
          '';
        };

      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          gapWasmSource = builtins.getEnv "SAPPHIRE_GAP_WASM_SOURCE";
          physicalWasmSource = builtins.getEnv "SAPPHIRE_PHYSICAL_WASM_SOURCE";
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
          ath11kReferenceSource = pkgs.callPackage ./nix/ath11k-reference-source.nix { };
          mt76ReferenceSource = pkgs.callPackage ./nix/mt76-reference-source.nix { };
        in
        rec {
          ath11k-reference-source = ath11kReferenceSource;
          mt76-reference-source = mt76ReferenceSource;
          mt7921-fuchsia-source = mt7921FuchsiaSource;
          mt7921-fuchsia-source-negative-tests = pkgs.runCommand
            "mt7921-fuchsia-source-negative-tests"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.findutils pkgs.gnutar pkgs.gnugrep pkgs.patch ]; }
            ''
              set -euo pipefail
              verify=${./nix/verify-mt7921-fuchsia-source.sh}
              source=${mt7921FuchsiaSource}/reference/fuchsia-${mt7921FuchsiaSource.fuchsiaBaseRevision}
              cp ${mt7921FuchsiaSource}/reference/fuchsia-${mt7921FuchsiaSource.fuchsiaBaseRevision}/.drv-host-patches expected
              base=${mt7921FuchsiaSource.fuchsiaBaseRevision}
              set_hash=${mt7921FuchsiaSource.fuchsiaOrderedPatchSetSha256}
              closure=$(cat "$source/.drv-source-closure")

              bash "$verify" "$source" "$base" expected "$set_hash" "$closure" derivation-owned
              cp -R "$source" stale
              chmod -R u+w stale
              printf '%s\n' a5386e3039ac3d04ee9fe387e317272f3c8d0aa72661e57f9c2c6c02f19c33db \
                > stale/.drv-host-patch-set
              ! bash "$verify" stale "$base" expected "$set_hash" "$closure" derivation-owned

              cp expected missing
              sed -i '/wlan-mlme-host.patch/d' missing
              ! bash "$verify" "$source" "$base" missing "$set_hash" "$closure" derivation-owned
              cp expected reordered
              sed -i '12,13{h;12d;13G}' reordered
              ! bash "$verify" "$source" "$base" reordered "$set_hash" "$closure" derivation-owned

              # The same bytes are rejected when labeled as an ignored
              # working-tree reference; acceptance requires derivation ownership.
              ! bash "$verify" "$source" "$base" expected "$set_hash" "$closure" ignored-reference

              mkdir -p offset/a
              printf 'inserted\nheader\nalpha\nbeta\ntail\n' > offset/a/file
              cat > offset.patch <<'EOF'
              --- a/a/file
              +++ b/a/file
              @@ -3,4 +3,4 @@
               header
              -alpha
              +changed
               beta
               tail
              EOF
              ! bash ${./nix/apply-exact-patch.sh} offset offset.patch offset.log
              grep -Fi offset offset.log
              touch "$out"
            '';
          wlan-control-wire =
            let
              mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
            in
            pkgs.rustPlatform.buildRustPackage {
              pname = "wlan-control-wire";
              version = "0.1.0";
              src = mt7921FuchsiaSource;
              cargoRoot = "crates/wifi/wlan-control-wire";
              buildAndTestSubdir = "crates/wifi/wlan-control-wire";
              cargoLock.lockFile = ./crates/wifi/wlan-control-wire/Cargo.lock;
              doCheck = false;
              installPhase = ''
                mkdir -p "$out/share/wlan-control-wire"
                cp crates/wifi/wlan-control-wire/Cargo.toml crates/wifi/wlan-control-wire/Cargo.lock \
                  "$out/share/wlan-control-wire/"
                cp -R crates/wifi/wlan-control-wire/src "$out/share/wlan-control-wire/"
              '';
            };
          wlancfg-service = pkgs.rustPlatform.buildRustPackage {
            pname = "wlancfg-service";
            version = "0.1.0";
            src = mt7921FuchsiaSource;
            cargoRoot = "crates/wifi/wlancfg-service";
            buildAndTestSubdir = "crates/wifi/wlancfg-service";
            cargoLock.lockFile = ./crates/wifi/wlancfg-service/Cargo.lock;
            nativeBuildInputs = [ pkgs.cmake pkgs.perl ];
            doCheck = false;
          };

          ath11k-wifi-service = pkgs.rustPlatform.buildRustPackage {
            pname = "ath11k-wifi-service";
            version = "0.1.0";
            src = mt7921FuchsiaSource;
            cargoRoot = "crates/wifi/drivers/ath11k/ath11k-wifi-service";
            buildAndTestSubdir = "crates/wifi/drivers/ath11k/ath11k-wifi-service";
            cargoLock.lockFile = ./crates/wifi/drivers/ath11k/ath11k-wifi-service/Cargo.lock;
            nativeBuildInputs = [ pkgs.cmake pkgs.perl ];
            doCheck = false;
            meta.mainProgram = "ath11k-wifi-service";
          };

          audio-pipewire-daemon = pkgs.callPackage ./lab/audio/audio-pipewire-spike/package.nix { };

          netstack3-provider-daemon = pkgs.callPackage ./crates/net/netstack3-port-spike/provider-package.nix { };

          hardware-backends = pkgs.rustPlatform.buildRustPackage {
            pname = "drv-hardware-backends";
            version = "0.1.0";
            src = builtins.path {
              path = ./.;
              name = "drv-source";
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "drv-hardware-backends"
              "--bin"
              "vfio_edu"
            ];
            cargoTestFlags = [
              "-p"
              "drv-hardware-backends"
            ];
          };

          ath11k-bringup-aarch64 =
            let
              cross = pkgs.pkgsCross.aarch64-multiplatform;
            in
            cross.pkgsStatic.rustPlatform.buildRustPackage {
              pname = "ath11k-bringup-aarch64";
              version = "0.1.0";
              src = builtins.path {
                path = ./.;
                name = "drv-source";
              };
              cargoLock.lockFile = ./Cargo.lock;
              cargoBuildFlags = [
                "-p"
                "ath11k-bringup"
                "--bin"
                "ath11k-bringup"
              ];
              doCheck = false;
              postInstall = ''
                test -x "$out/bin/ath11k-bringup"
                ${cross.stdenv.cc.bintools.bintools}/bin/${cross.stdenv.cc.targetPrefix}readelf -h \
                  "$out/bin/ath11k-bringup" | grep -F 'Machine:' | grep -F 'AArch64'
                ! ${cross.stdenv.cc.bintools.bintools}/bin/${cross.stdenv.cc.targetPrefix}readelf -l \
                  "$out/bin/ath11k-bringup" | grep -F 'Requesting program interpreter'
              '';
              nativeBuildInputs = [ pkgs.gnugrep ];
              meta.mainProgram = "ath11k-bringup";
            };

          ath11k-wifi-service-aarch64 =
            let
              cross = pkgs.pkgsCross.aarch64-multiplatform;
            in
            cross.pkgsStatic.rustPlatform.buildRustPackage {
              pname = "ath11k-wifi-service-aarch64";
              version = "0.1.0";
              src = mt7921FuchsiaSource;
              cargoRoot = "crates/wifi/drivers/ath11k/ath11k-wifi-service";
              buildAndTestSubdir = "crates/wifi/drivers/ath11k/ath11k-wifi-service";
              cargoLock.lockFile = ./crates/wifi/drivers/ath11k/ath11k-wifi-service/Cargo.lock;
              cargoBuildFlags = [ "--bins" ];
              cargoInstallFlags = [ "--bins" ];
              nativeBuildInputs = [ pkgs.cmake pkgs.perl pkgs.gnugrep ];
              doCheck = false;
              postInstall = ''
                test -x "$out/bin/ath11k-wifi-service"
                test -x "$out/bin/redwood-wifi-diagnostic"
                ${cross.stdenv.cc.bintools.bintools}/bin/${cross.stdenv.cc.targetPrefix}readelf -h \
                  "$out/bin/ath11k-wifi-service" | grep -F 'Machine:' | grep -F 'AArch64'
                ${cross.stdenv.cc.bintools.bintools}/bin/${cross.stdenv.cc.targetPrefix}readelf -h \
                  "$out/bin/redwood-wifi-diagnostic" | grep -F 'Machine:' | grep -F 'AArch64'
                ! ${cross.stdenv.cc.bintools.bintools}/bin/${cross.stdenv.cc.targetPrefix}readelf -l \
                  "$out/bin/ath11k-wifi-service" | grep -F 'Requesting program interpreter'
                ! ${cross.stdenv.cc.bintools.bintools}/bin/${cross.stdenv.cc.targetPrefix}readelf -l \
                  "$out/bin/redwood-wifi-diagnostic" | grep -F 'Requesting program interpreter'
              '';
              meta.mainProgram = "ath11k-wifi-service";
            };

          # Reduced service: firmware and inherited WLCP endpoints are supplied
          # by the lifecycle owner, not credentials or legacy lab snapshots.
          # Both firmware images are uncompressed and hash-checked before VFIO.
          mt7921-wifi-service = pkgs.rustPlatform.buildRustPackage {
            pname = "mt7921-wifi-service";
            version = "0.1.0";
            src = mt7921FuchsiaSource;
            cargoRoot = "crates/wifi/drivers/mt7921/mt7921-passive-scan";
            buildAndTestSubdir = "crates/wifi/drivers/mt7921/mt7921-passive-scan";
            cargoLock.lockFile = ./crates/wifi/drivers/mt7921/mt7921-passive-scan/Cargo.lock;
            cargoBuildFlags = [ "--bin" "mt7921-passive-scan" ];
            cargoInstallFlags = [ "--bin" "mt7921-passive-scan" ];
            nativeBuildInputs = [ pkgs.cmake pkgs.perl ];
            doCheck = false;
            postInstall = ''
              mv "$out/bin/mt7921-passive-scan" "$out/bin/mt7921-wifi-service"
            '';
            # Launch with --run-wifi-service (the lifecycle launcher owns args).
            # Caller must provide DRV_MT7921_PATCH_IMAGE / DRV_MT7921_RAM_IMAGE,
            # generation, NIC MAC, policy/supervisor FDs and device identity.
            # The service retains the armed-watchdog activation gate.
            meta.mainProgram = "mt7921-wifi-service";
          };

          bluetooth-sapphire-runner = pkgs.rustPlatform.buildRustPackage {
            pname = "bluetooth-sapphire-runner";
            version = "0.1.0";
            src = builtins.path {
              path = ./.;
              name = "drv-source";
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "bluetooth-sapphire-wasm"
            ];
            cargoTestFlags = [
              "-p"
              "bluetooth-sapphire-wasm"
            ];
            postInstall = ''
              mkdir -p "$out/share/bluetooth-sapphire"
              cat >"$out/share/bluetooth-sapphire/systemd.properties" <<'EOF'
              RuntimeMaxSec=150s
              TimeoutStopSec=5s
              MemoryHigh=768M
              MemoryMax=1G
              CPUQuota=100%
              TasksMax=8
              LimitNOFILE=64
              KillMode=control-group
              OOMPolicy=kill
              NoNewPrivileges=yes
              ProtectSystem=strict
              ProtectHome=yes
              ProtectKernelTunables=yes
              ProtectKernelModules=yes
              ProtectControlGroups=yes
              RestrictAddressFamilies=AF_UNIX AF_BLUETOOTH
              CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_SETUID CAP_SETGID CAP_KILL
              LockPersonality=yes
              RestrictRealtime=yes
              RestrictSUIDSGID=yes
              SystemCallArchitectures=native
              UMask=0077
              EOF
            '';
          };

          sapphire-gap-wasm = pkgs.runCommand "sapphire-gap-test.wasm" { } (
            if gapWasmSource == "" then
              ''
                echo "set SAPPHIRE_GAP_WASM_SOURCE and evaluate with --impure" >&2
                exit 1
              ''
            else
              let
                source = builtins.path {
                  path = gapWasmSource;
                  name = "sapphire-gap-test.wasm.source";
                };
              in
              ''
                test "$(${pkgs.coreutils}/bin/sha256sum ${source} | ${pkgs.coreutils}/bin/cut -d ' ' -f 1)" = \
                  c988088b7e30d5c1ce2795d3b61d89700d2a252ff3750b75d2f0c6f3c857b93f
                cp ${source} "$out"
              ''
          );

          sapphire-physical-discovery-wasm = pkgs.runCommand "sapphire-physical-discovery.wasm" { } (
            if physicalWasmSource == "" then
              ''
                echo "set SAPPHIRE_PHYSICAL_WASM_SOURCE and evaluate with --impure" >&2
                exit 1
              ''
            else
              let
                source = builtins.path {
                  path = physicalWasmSource;
                  name = "sapphire-physical-discovery.wasm.source";
                };
              in
              ''
                test "$(${pkgs.coreutils}/bin/sha256sum ${source} | ${pkgs.coreutils}/bin/cut -d ' ' -f 1)" = \
                  e8aa718f071332e6b71649857b2e41651dc002d12ae1acbab08482102762358c
                cp ${source} "$out"
              ''
          );

          sapphire-discovery = pkgs.writeShellApplication {
            name = "bluetooth-sapphire-discover";
            text = ''
              runner=${bluetooth-sapphire-runner}/bin/bluetooth-sapphire-wasm
              case ''${1-} in
                --restore-controller-state|--probe-user-channel)
                  exec "$runner" "$@"
                  ;;
              esac
              if (( $# != 8 )) || [[ $1 != --device || $3 != --seconds || $5 != --report || $7 != --state ]]; then
                echo "usage: bluetooth-sapphire-discover --device N --seconds N --report ABSOLUTE --state ABSOLUTE" >&2
                exit 2
              fi
              exec "$runner" --physical-discovery ${sapphire-physical-discovery-wasm} \
                "$@" --confirm-discovery-only
            '';
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # Keep this aligned with scripts/fetch-fuchsia-reference.
          sapphirePigweedArchive = pkgs.fetchurl {
            url = "https://pigweed.googlesource.com/pigweed/pigweed/+archive/c14c119c51a82f6e044f81b7dad0a322091d4121.tar.gz";
            hash = "sha256-VhMYiubataca+o9sZP3SkLxSdNPisZPP8dG+4jnVscQ=";
          };
        in
        {
          # Minimal shell for incremental out-of-nix builds of the MT7921
          # validation binary on the lab host (lab/fast-build.sh). Uses the same
          # rustc/cargo as `rustPlatform.buildRustPackage`, so the artifact only
          # differs from the nix build by its identity string.
          mt7921 = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              cmake
              pkg-config
              perl
              rsync
            ];
          };
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              cargo-component
              bazel_8
              clippy
              cmake
              lld
              pkg-config
              python3
              rustc
              rustfmt
              wasmtime
              wasm-tools

              # Sapphire is compiled as C++ inside WebAssembly; no Sapphire
              # object code is linked into a native target.
              pkgsCross.wasi32.stdenv.cc

              # Required by bindgen-based crates that consume Linux VFIO headers.
              linuxHeaders
              rustPlatform.bindgenHook
            ];

            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.linuxHeaders}/include";
            SAPPHIRE_PIGWEED_ARCHIVE = sapphirePigweedArchive;
          };
        }
      );
    };
}
