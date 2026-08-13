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
      nixosModules.netstack3-kernel-provider =
        import ./crates/netstack3-port-spike/kernel-provider/module.nix;

      checks.x86_64-linux.vfio-edu =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./nix/vfio-edu-test.nix
          { };

      checks.x86_64-linux.netstack3-kernel-provider =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/netstack3-port-spike/kernel-provider/check.nix
          { };

      checks.x86_64-linux.audio-pipewire-daemon =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/audio-pipewire-spike/package.nix
          { };

      checks.x86_64-linux.netstack3-provider-daemon =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/netstack3-port-spike/provider-package.nix
          { };

      checks.x86_64-linux.netstack3-provider-service =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/netstack3-port-spike/kernel-provider/service-test.nix
          { };

      checks.x86_64-linux.netstack3-kernel-provider-boot =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/netstack3-port-spike/kernel-provider/boot-test.nix
          { };

      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          gapWasmSource = builtins.getEnv "SAPPHIRE_GAP_WASM_SOURCE";
          physicalWasmSource = builtins.getEnv "SAPPHIRE_PHYSICAL_WASM_SOURCE";
        in
        rec {
          audio-pipewire-daemon =
            pkgs.callPackage ./crates/audio-pipewire-spike/package.nix { };

          netstack3-provider-daemon =
            pkgs.callPackage ./crates/netstack3-port-spike/provider-package.nix
              { };

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

          mt7921-patch-table-gate = pkgs.stdenv.mkDerivation {
            pname = "mt7921-patch-table-gate";
            version = "0.1.0";
            src =
              let gateBinary = builtins.getEnv "MT7921_GATE_BINARY";
              in
              if gateBinary == "" then
                throw "set MT7921_GATE_BINARY to the exact locally verified release executable and evaluate with --impure"
              else
                builtins.path {
                  path = gateBinary;
                  name = "mt7921-patch-table-gate-unpatched";
                };
            nativeBuildInputs = [ pkgs.autoPatchelfHook pkgs.binutils ];
            buildInputs = [ pkgs.stdenv.cc.cc.lib ];
            dontUnpack = true;
            installPhase = ''
              runHook preInstall
              install -Dm0755 "$src" "$out/bin/mt7921-passive-scan"
              runHook postInstall
            '';
            doInstallCheck = true;
            installCheckPhase = ''
              runHook preInstallCheck
              strings $out/bin/mt7921-passive-scan | grep -F 'external reboot watchdog is not armed'
              strings $out/bin/mt7921-passive-scan | grep -F '"watchdog_verified":true'
              gate_output="$($out/bin/mt7921-passive-scan --self-test-patch-table-gate)"
              test "$(printf '%s\n' "$gate_output" | grep -c '"patch_gate_transcript":"command"')" -eq 4
              test "$(printf '%s\n' "$gate_output" | grep -c '"patch_gate_transcript":"scatter"')" -eq 23
              printf '%s\n' "$gate_output" | grep -F '"patch_gate_result":"passed"' | grep -F '"row_count":41' | grep -F '"before_ram_cmd_0x01":true'
              printf '%s\n' "$gate_output" | grep -F '"patch_gate_self_test":"passed","mmio_reads":41,"ram_operations":0,"cleanup_state":"Ready"'
              runHook postInstallCheck
            '';
            meta.mainProgram = "mt7921-passive-scan";
          };

          mt7921-full-firmware-validation = pkgs.stdenv.mkDerivation {
            pname = "mt7921-full-firmware-validation";
            version = "0.1.0";
            src =
              let fullFirmwareBinary = builtins.getEnv "MT7921_FULL_FIRMWARE_BINARY";
              in
              if fullFirmwareBinary == "" then
                throw "set MT7921_FULL_FIRMWARE_BINARY to the exact locally verified release executable and evaluate with --impure"
              else
                builtins.path {
                  path = fullFirmwareBinary;
                  name = "mt7921-full-firmware-validation-unpatched";
                };
            nativeBuildInputs = [ pkgs.autoPatchelfHook pkgs.binutils ];
            buildInputs = [ pkgs.stdenv.cc.cc.lib ];
            dontUnpack = true;
            installPhase = ''
              runHook preInstall
              install -Dm0755 "$src" "$out/libexec/mt7921-full-firmware-validation"
              mkdir -p "$out/bin"
              ln -s ../libexec/mt7921-full-firmware-validation "$out/bin/mt7921-full-firmware-validation-driver"
              substitute ${./nix/mt7921-full-firmware-validation-launcher.sh} \
                "$out/bin/mt7921-full-firmware-validation" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by driver "$out/libexec/mt7921-full-firmware-validation" \
                --subst-var-by credential_file /var/lib/iwd/ph1.psk \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by env ${pkgs.coreutils}/bin/env
              chmod 0755 "$out/bin/mt7921-full-firmware-validation"
              runHook postInstall
            '';
            doInstallCheck = true;
            installCheckPhase = ''
              runHook preInstallCheck
              driver=$out/libexec/mt7921-full-firmware-validation
              strings "$driver" | grep -F '"full_firmware_preflight":"passed"'
              strings "$driver" | grep -F 'ram_published_firmware_start_acked'
              strings "$driver" | grep -F 'post_release_before_ram'
              strings "$driver" | grep -F 'immediately_before_rx_path'
              strings "$driver" | grep -F 'after_rate_power_final'
              strings "$driver" | grep -F 'immediately_predata'
              strings "$driver" | grep -F 'e2e94_tx_success_gate result='
              strings "$driver" | grep -F 'stop_after_one=true eapol_published=false vo_published=false second_frame_published=false retry_published=false'
              rate_output="$("$driver" --self-test-rate-power-delivery)"
              test "$(printf '%s\n' "$rate_output" | grep -c '"rate_power_self_test":"command"')" -eq 10
              printf '%s\n' "$rate_output" \
                | grep -F '"rate_power_self_test":"passed"' \
                | grep -F '"audit":"hardware_post_dma_consumption_reclaim"' \
                | grep -F '"total_lengths":"1404,1080,1404,1404,1404,1404,1404,1404"' \
                | grep -F '"raw_lengths":"1340,1016,1340,1340,1340,1340,1340,1340"' \
                | grep -F '"sequences":"15,1,2,3,4,5,6,7"' \
                | grep -F '"reg_read_between_pages":0' \
                | grep -F '"safe_reclaims":8'
              launcher=$out/bin/mt7921-full-firmware-validation
              grep -F 'case "$#:''${1-}" in' "$launcher"
              grep -F 'DRV_E2E94_EDCA_PROBE=1' "$launcher"
              grep -F 'DRV_SAE_BSSID=72:a6:c7:7d:56:93' "$launcher"
              grep -F 'DRV_SAE_CHANNEL=36' "$launcher"
              grep -F 'DRV_SAE_SSID=ph1' "$launcher"
              grep -F 'DRV_SAE_CLIENT_MAC=8a:fd:2a:8b:70:5a' "$launcher"
              grep -F -- '--run-one-shot-sae-auth' "$launcher"
              test "$(grep -Fc 'exec ' "$launcher")" -eq 3
              if "$out/bin/mt7921-full-firmware-validation" --run-one-shot-patch-table-gate 2>/dev/null; then
                echo 'fixed launcher unexpectedly accepted patch-table gate dispatch' >&2
                exit 1
              fi
              runHook postInstallCheck
            '';
            meta.mainProgram = "mt7921-full-firmware-validation";
          };

          mt7921-full-firmware-validation-launcher-test = pkgs.runCommand
            "mt7921-full-firmware-validation-launcher-test"
            { nativeBuildInputs = [ pkgs.coreutils pkgs.gnused ]; }
            ''
              mkdir -p work/bin work/var
              cat > work/bin/validation-stub <<'EOF'
              #!${pkgs.runtimeShell}
              set -eu
              transcript=$PWD/transcript
              printf 'ARGV' > "$transcript"
              printf ' <%s>' "$@" >> "$transcript"
              printf '\n' >> "$transcript"
              ${pkgs.coreutils}/bin/env | ${pkgs.coreutils}/bin/sort >> "$transcript"
              credential=$(${pkgs.coreutils}/bin/cat <&3)
              printf 'CREDENTIAL_LEN=%s\n' "''${#credential}" >> "$transcript"
              EOF
              chmod 0755 work/bin/validation-stub
              printf 'Passphrase=eight-by\n' > work/var/ph1.psk
              substitute ${./nix/mt7921-full-firmware-validation-launcher.sh} work/launcher \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by driver "$PWD/work/bin/validation-stub" \
                --subst-var-by credential_file "$PWD/work/var/ph1.psk" \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by env ${pkgs.coreutils}/bin/env
              chmod 0755 work/launcher
              env -i \
                DRV_PCI_BDF=0000:05:00.0 DRV_IOMMU_GROUP=17 \
                DRV_VFIO_DEVICE=/dev/vfio/devices/vfio17 \
                DRV_LAB_SAFETY_STATE=/run/wifi-driver-lab/fixed.state.safety \
                work/launcher
              grep -Fx 'ARGV <--run-one-shot-sae-auth>' transcript
              grep -Fx 'DRV_E2E94_EDCA_PROBE=1' transcript
              grep -Fx 'DRV_SAE_BSSID=72:a6:c7:7d:56:93' transcript
              grep -Fx 'DRV_SAE_CHANNEL=36' transcript
              grep -Fx 'DRV_SAE_SSID=ph1' transcript
              grep -Fx 'DRV_SAE_CLIENT_MAC=8a:fd:2a:8b:70:5a' transcript
              grep -Fx 'DRV_SAE_CREDENTIAL_FD=3' transcript
              grep -Fx 'DRV_SAE_CREDENTIAL_LEN=8' transcript
              grep -Fx 'CREDENTIAL_LEN=8' transcript
              ! grep -q 'PATCH_TABLE' transcript
              ! grep -q 'EAPOL' transcript
              cp transcript "$out"
            '';

          mt7921-full-firmware-validation-manifest =
            let
              package = mt7921-full-firmware-validation;
              closure = pkgs.closureInfo { rootPaths = [ package ]; };
            in
            pkgs.runCommand "mt7921-full-firmware-validation-manifest"
              { nativeBuildInputs = [ pkgs.coreutils ]; }
              ''
                launcher=${package}/bin/mt7921-full-firmware-validation
                driver=${package}/bin/mt7921-full-firmware-validation-driver
                closure_sha=$(sort ${closure}/store-paths | sha256sum | cut -d ' ' -f1)
                cat > "$out" <<EOF
                PACKAGE=${package}
                LAUNCHER=$launcher
                LAUNCHER_SHA256=$(sha256sum "$launcher" | cut -d ' ' -f1)
                ELF=$driver
                ELF_SHA256=$(sha256sum "$driver" | cut -d ' ' -f1)
                CLOSURE_SHA256=$closure_sha
                PCI_BDF=0000:05:00.0
                TIMEOUT_SECONDS=300
                OPERATION=--run-one-shot-sae-auth
                MODE=DRV_E2E94_EDCA_PROBE=1
                TARGET_SSID=ph1
                TARGET_BSSID=72:a6:c7:7d:56:93
                TARGET_CHANNEL=36
                TARGET_CLIENT_MAC=8a:fd:2a:8b:70:5a
                FRAME=qos_null_tid0_be_qidx1
                SUCCESS=tx_free_status_0_count_1_and_correlated_txs_ack
                PATCH_TABLE_SNAPSHOTS=before_rx_path_after_each_of_8_pages_and_after_final
                PATCH_TABLE_REQUIRED_BEFORE_ADD_DEVICE=41_of_41
                RATE_POWER_ORDER=rx_path_then_8_contiguous_0x4005d_then_add_device
                RATE_POWER_REG_READ_BETWEEN_PAGES=0
                RATE_POWER_LAST_MSG_PAGE=8
                PATCH_TABLE_GATE=false
                STOP_AFTER_ONE=true
                EAPOL_START=false
                VO_PROBE=false
                SECOND_FRAME=false
                RETRY=false
                EOF
              '';

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

          sapphire-physical-discovery-wasm =
            pkgs.runCommand "sapphire-physical-discovery.wasm" { } (
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
