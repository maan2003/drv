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
      nixosModules.netstack3-kernel-provider = import ./crates/netstack3-port-spike/kernel-provider/module.nix;

      checks.x86_64-linux.vfio-edu =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./nix/vfio-edu-test.nix
          { };

      checks.x86_64-linux.netstack3-kernel-provider =
        nixpkgs.legacyPackages.x86_64-linux.callPackage
          ./crates/netstack3-port-spike/kernel-provider/check.nix
          { };

      checks.x86_64-linux.audio-pipewire-daemon =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./crates/audio-pipewire-spike/package.nix
          { };

      checks.x86_64-linux.netstack3-provider-daemon =
        nixpkgs.legacyPackages.x86_64-linux.callPackage ./crates/netstack3-port-spike/provider-package.nix
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
          regulatoryDb = pkgs.runCommand "wireless-regdb-v20-uncompressed" {
            nativeBuildInputs = [ pkgs.zstd ];
          } ''
            if test -f ${pkgs.wireless-regdb}/lib/firmware/regulatory.db; then
              cp ${pkgs.wireless-regdb}/lib/firmware/regulatory.db "$out"
            else
              zstd -dc ${pkgs.wireless-regdb}/lib/firmware/regulatory.db.zst > "$out"
            fi
          '';
        in
        rec {
          audio-pipewire-daemon = pkgs.callPackage ./crates/audio-pipewire-spike/package.nix { };

          netstack3-provider-daemon = pkgs.callPackage ./crates/netstack3-port-spike/provider-package.nix { };

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
              let
                gateBinary = builtins.getEnv "MT7921_GATE_BINARY";
              in
              if gateBinary == "" then
                throw "set MT7921_GATE_BINARY to the exact locally verified release executable and evaluate with --impure"
              else
                builtins.path {
                  path = gateBinary;
                  name = "mt7921-patch-table-gate-unpatched";
                };
            nativeBuildInputs = [
              pkgs.autoPatchelfHook
              pkgs.binutils
            ];
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

          mt7921-rate-power-evidence = pkgs.stdenv.mkDerivation {
            pname = "mt7921-rate-power-evidence";
            version = "0.1.0";
            src =
              let
                evidenceBinary = builtins.getEnv "MT7921_RATE_POWER_EVIDENCE_BINARY";
              in
              if evidenceBinary == "" then
                throw "set MT7921_RATE_POWER_EVIDENCE_BINARY to the exact locally verified release executable and evaluate with --impure"
              else
                builtins.path {
                  path = evidenceBinary;
                  name = "mt7921-rate-power-evidence-unpatched";
                };
            sourceCommit =
              let value = builtins.getEnv "MT7921_RATE_POWER_EVIDENCE_SOURCE_COMMIT";
              in
              if builtins.match "[0-9a-f]{40}" value == null then
                throw "set MT7921_RATE_POWER_EVIDENCE_SOURCE_COMMIT to the 40-hex source commit embedded in the evidence executable"
              else
                value;
            nativeBuildInputs = [
              pkgs.autoPatchelfHook
              pkgs.binutils
            ];
            buildInputs = [ pkgs.stdenv.cc.cc.lib ];
            dontUnpack = true;
            installPhase = ''
              install -Dm0755 "$src" "$out/libexec/mt7921-rate-power-evidence"
              mkdir -p "$out/share/mt7921-rate-power-evidence"
              cat > "$out/share/mt7921-rate-power-evidence/artifact-identity.json" <<EOF
              {"artifact_identity":"mt7921-validation-v1","flavor":"rate-power-evidence-only","enabled_operation":"run-one-shot-power-setup","source_commit":"$sourceCommit","fd_contract":"credential-fd3+snapshot-fd4+immediate-eof","active_capable":false}
              EOF
              mkdir -p "$out/bin"
              regulatory_source_sha256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
              substitute ${./nix/mt7921-rate-power-evidence-launcher.sh} \
                "$out/bin/mt7921-rate-power-evidence" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by driver "$out/libexec/mt7921-rate-power-evidence" \
                --subst-var-by snapshot_generator "$out/libexec/mt7921-rate-power-evidence" \
                --subst-var-by regulatory_db ${regulatoryDb} \
                --subst-var-by regulatory_source_sha256 "$regulatory_source_sha256" \
                --subst-var-by credential_file /var/lib/iwd/ph1.psk \
                --subst-var-by artifact_identity "$out/share/mt7921-rate-power-evidence/artifact-identity.json" \
                --subst-var-by cat ${pkgs.coreutils}/bin/cat \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                --subst-var-by wc ${pkgs.coreutils}/bin/wc \
                --subst-var-by stat ${pkgs.coreutils}/bin/stat \
                --subst-var-by rm ${pkgs.coreutils}/bin/rm \
                --subst-var-by env ${pkgs.coreutils}/bin/env
              chmod 0755 "$out/bin/mt7921-rate-power-evidence"
            '';
            postFixup = ''
              evidence_dir=$out/share/mt7921-rate-power-evidence
              mkdir -p "$evidence_dir"
              driver=$out/libexec/mt7921-rate-power-evidence
              "$driver" --artifact-identity > actual-identity.json
              cmp actual-identity.json "$evidence_dir/artifact-identity.json"
              "$driver" --self-test-rate-power-delivery > "$evidence_dir/offline-self-test.jsonl"
              "$driver" --generate-regulatory-snapshot-v20 ${regulatoryDb} 00 \
                2fb33ca0074db573e05ef7dd50bb45b63c0ff98b7e852e1105ebad536fae8e6b \
                > "$evidence_dir/regulatory.snapshot"
              cat > "$evidence_dir/ARTIFACTS" <<EOF
              OFFLINE_SELF_TEST_SHA256=$(sha256sum "$evidence_dir/offline-self-test.jsonl" | cut -d ' ' -f1)
              REGULATORY_SNAPSHOT_SHA256=$(sha256sum "$evidence_dir/regulatory.snapshot" | cut -d ' ' -f1)
              REGULATORY_SNAPSHOT_BYTES=$(wc -c < "$evidence_dir/regulatory.snapshot")
              REGULATORY_SOURCE_SHA256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
              COUNTRY=00
              GENERATION=0
              EOF
            '';
            doInstallCheck = true;
            installCheckPhase = ''
              driver=$out/libexec/mt7921-rate-power-evidence
              test "$("$driver" --artifact-identity)" = "$(cat $out/share/mt7921-rate-power-evidence/artifact-identity.json)"
              grep -F '"flavor":"rate-power-evidence-only"' $out/share/mt7921-rate-power-evidence/artifact-identity.json
              grep -F '"active_capable":false' $out/share/mt7921-rate-power-evidence/artifact-identity.json
              strings "$driver" | grep -F 'rate_power_publication'
              strings "$driver" | grep -F 'rate_power_evidence_stop'
              strings "$driver" | grep -F 'native_golden_match=true'
              strings "$driver" | grep -F 'evidence-only build permits only pinned-regdb rate-power delivery'
              output=$("$driver" --self-test-rate-power-delivery)
              test "$(printf '%s\n' "$output" | grep -c '"rate_power_self_test":"command"')" -eq 10
              printf '%s\n' "$output" | grep -F '"rate_power_self_test":"passed"'
              for hash in \
                a518536c96d2de1a8ba398cbd0fbdb1b00f5b90434d27e8de2fefb97b2ba7b95 \
                1c365518ffebb7a2b12b924bcfbe41436b4f2d52a83b07682d6f16c79eb39db0 \
                1cbc40088bc367d75b2119806d3a8114ed3c4ce92a9572f262faace837343d36 \
                f03e59182fd1e605df82de38305d445a015c9fc44c12da7802132514fed3e4d5 \
                231e8db12ba160bdc12af55ef61c2da091387951c029008cc68b6b8d8a71d208 \
                d8761b04f27de826280c55aac39a96e4d3bc0bb4d83330d910a4fd3ca70b90b2 \
                e018434f160c1b67dc86477c602a87627b5553c5304359912347a04cbb3aad44 \
                f1e5d489bb579d4d080eb8c2569e752c0b0dd87ccbdf02a112705536791d89df
              do
                printf '%s\n' "$output" | grep -F "$hash"
              done
              "$driver" --generate-regulatory-snapshot-v20 ${regulatoryDb} 00 \
                2fb33ca0074db573e05ef7dd50bb45b63c0ff98b7e852e1105ebad536fae8e6b > snapshot
              test "$(wc -c < snapshot)" -eq 580
              test "$(wc -c < $out/share/mt7921-rate-power-evidence/regulatory.snapshot)" -eq 580
              cmp snapshot $out/share/mt7921-rate-power-evidence/regulatory.snapshot
              grep -F '"rate_power_self_test":"passed"' \
                $out/share/mt7921-rate-power-evidence/offline-self-test.jsonl
              if "$driver" --run-one-shot-sae-auth 2>error; then exit 1; fi
              grep -F 'evidence-only build' error
              if "$out/bin/mt7921-rate-power-evidence" --run-one-shot-sae-auth 2>error; then exit 1; fi
              grep -F 'accepts no arguments' error
            '';
            meta.mainProgram = "mt7921-rate-power-evidence";
          };

          mt7921-full-firmware-validation = pkgs.stdenv.mkDerivation {
            pname = "mt7921-full-firmware-validation";
            version = "0.1.0";
            src =
              let
                fullFirmwareBinary = builtins.getEnv "MT7921_FULL_FIRMWARE_BINARY";
              in
              if fullFirmwareBinary == "" then
                throw "set MT7921_FULL_FIRMWARE_BINARY to the exact locally verified release executable and evaluate with --impure"
              else
                builtins.path {
                  path = fullFirmwareBinary;
                  name = "mt7921-full-firmware-validation-unpatched";
                };
            sourceCommit =
              let value = builtins.getEnv "MT7921_FULL_FIRMWARE_SOURCE_COMMIT";
              in
              if builtins.match "[0-9a-f]{40}" value == null then
                throw "set MT7921_FULL_FIRMWARE_SOURCE_COMMIT to the 40-hex source commit embedded in the production executable"
              else
                value;
            nativeBuildInputs = [
              pkgs.autoPatchelfHook
              pkgs.binutils
            ];
            buildInputs = [ pkgs.stdenv.cc.cc.lib ];
            dontUnpack = true;
            installPhase = ''
              runHook preInstall
              install -Dm0755 "$src" "$out/libexec/mt7921-full-firmware-validation"
              mkdir -p "$out/share/mt7921-full-firmware-validation"
              cat > "$out/share/mt7921-full-firmware-validation/artifact-identity.json" <<EOF
              {"artifact_identity":"mt7921-validation-v1","flavor":"full-firmware-production","enabled_operation":"run-one-shot-sae-auth","source_commit":"$sourceCommit","fd_contract":"credential-fd3+snapshot-fd4+immediate-eof","active_capable":true}
              EOF
              cat > "$out/share/mt7921-full-firmware-validation/mock-ph1.psk" <<'EOF'
              Passphrase=packaged-integration-only
              EOF
              regulatory_source_sha256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
              test "$regulatory_source_sha256" = 2fb33ca0074db573e05ef7dd50bb45b63c0ff98b7e852e1105ebad536fae8e6b
              mkdir -p "$out/bin"
              ln -s ../libexec/mt7921-full-firmware-validation "$out/bin/mt7921-full-firmware-validation-driver"
              substitute ${./nix/mt7921-full-firmware-validation-launcher.sh} \
                "$out/bin/mt7921-full-firmware-validation" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by driver "$out/libexec/mt7921-full-firmware-validation" \
                --subst-var-by snapshot_generator "$out/libexec/mt7921-full-firmware-validation" \
                --subst-var-by regulatory_db ${regulatoryDb} \
                --subst-var-by regulatory_source_sha256 "$regulatory_source_sha256" \
                --subst-var-by credential_file /var/lib/iwd/ph1.psk \
                --subst-var-by mock_credential_file "$out/share/mt7921-full-firmware-validation/mock-ph1.psk" \
                --subst-var-by artifact_identity "$out/share/mt7921-full-firmware-validation/artifact-identity.json" \
                --subst-var-by cat ${pkgs.coreutils}/bin/cat \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                --subst-var-by wc ${pkgs.coreutils}/bin/wc \
                --subst-var-by stat ${pkgs.coreutils}/bin/stat \
                --subst-var-by rm ${pkgs.coreutils}/bin/rm \
                --subst-var-by env ${pkgs.coreutils}/bin/env
              chmod 0755 "$out/bin/mt7921-full-firmware-validation"
              runHook postInstall
            '';
            postFixup = ''
              evidence_dir=$out/share/mt7921-full-firmware-validation
              mkdir -p "$evidence_dir"
              driver=$out/libexec/mt7921-full-firmware-validation
              "$driver" --artifact-identity > actual-identity.json
              cmp actual-identity.json "$evidence_dir/artifact-identity.json"
              "$driver" --self-test-rate-power-delivery > "$evidence_dir/rate-power-self-test.jsonl"
              "$driver" --self-test-production-validation > "$evidence_dir/production-self-test.jsonl"
              cat > "$evidence_dir/ARTIFACTS" <<EOF
              RATE_POWER_SELF_TEST_SHA256=$(sha256sum "$evidence_dir/rate-power-self-test.jsonl" | cut -d ' ' -f1)
              PRODUCTION_SELF_TEST_SHA256=$(sha256sum "$evidence_dir/production-self-test.jsonl" | cut -d ' ' -f1)
              REGULATORY_SOURCE_SHA256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
              REGULATORY_GENERATION=0
              EOF
            '';
            doInstallCheck = true;
            installCheckPhase = ''
              runHook preInstallCheck
              driver=$out/libexec/mt7921-full-firmware-validation
              test "$("$driver" --artifact-identity)" = "$(cat $out/share/mt7921-full-firmware-validation/artifact-identity.json)"
              grep -F '"flavor":"full-firmware-production"' $out/share/mt7921-full-firmware-validation/artifact-identity.json
              grep -F '"active_capable":true' $out/share/mt7921-full-firmware-validation/artifact-identity.json
              strings "$driver" | grep -F '"full_firmware_preflight":"passed"'
              strings "$driver" | grep -F 'ram_published_firmware_start_acked'
              strings "$driver" | grep -F 'post_release_before_ram'
              strings "$driver" | grep -F 'immediately_before_rx_path'
              strings "$driver" | grep -F 'after_rate_power_final'
              strings "$driver" | grep -F 'immediately_predata'
              strings "$driver" | grep -F 'e2e94_tx_success_gate result='
              strings "$driver" | grep -F 'stop_after_one=true eapol_published=false vo_published=false second_frame_published=false retry_published=false'
              strings "$driver" | grep -F 'production_policy_validation result=pass'
              strings "$driver" | grep -F 'tmac_population_invariant=false'
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
              production_output="$("$driver" --self-test-production-validation)"
              printf '%s\n' "$production_output" \
                | grep -F '"production_validation_self_test":"passed"' \
                | grep -F '"tx_free_status":0' \
                | grep -F '"tx_free_count":1' \
                | grep -F '"txs_acked":true' \
                | grep -F '"second_frame":false' \
                | grep -F '"tmac_population_invariant":false'
              integration_output="$(MT7921_PACKAGED_INTEGRATION_TEST=1 "$out/bin/mt7921-full-firmware-validation")"
              printf '%s\n' "$integration_output" \
                | grep -F '"packaged_zero_arg_integration":"passed"' \
                | grep -F '"dispatch":"normal-full-firmware-sae"' \
                | grep -F '"fd3_eof":true' \
                | grep -F '"fd4_eof":true' \
                | grep -F '"typed_binding_consumed":true' \
                | grep -F '"rate_power_pages":8' \
                | grep -F '"add_device_acked":true' \
                | grep -F '"frame":"qos_null_tid0_be"' \
                | grep -F '"device_opened":false' \
                | grep -F '"vfio_opened":false'
              launcher=$out/bin/mt7921-full-firmware-validation
              grep -F 'case "$#:''${1-}" in' "$launcher"
              grep -F 'DRV_E2E94_EDCA_PROBE=1' "$launcher"
              grep -F 'DRV_SAE_BSSID=72:a6:c7:7d:56:93' "$launcher"
              grep -F 'DRV_SAE_CHANNEL=36' "$launcher"
              grep -F 'DRV_SAE_SSID=ph1' "$launcher"
              grep -F 'DRV_SAE_CLIENT_MAC=8a:fd:2a:8b:70:5a' "$launcher"
              grep -F 'DRV_REGULATORY_SNAPSHOT_FD=4' "$launcher"
              grep -F 'DRV_REGULATORY_SNAPSHOT_LEN=' "$launcher"
              grep -F 'DRV_REGULATORY_SOURCE_SHA256=' "$launcher"
              grep -F -- '--generate-regulatory-snapshot-v20' "$launcher"
              grep -F -- '--run-one-shot-sae-auth' "$launcher"
              test "$(grep -Fc 'exec ' "$launcher")" -eq 5
              if "$out/bin/mt7921-full-firmware-validation" --run-one-shot-patch-table-gate 2>/dev/null; then
                echo 'fixed launcher unexpectedly accepted patch-table gate dispatch' >&2
                exit 1
              fi
              runHook postInstallCheck
            '';
            meta.mainProgram = "mt7921-full-firmware-validation";
          };

          mt7921-full-firmware-validation-launcher-test =
            pkgs.runCommand "mt7921-full-firmware-validation-launcher-test"
              {
                nativeBuildInputs = [
                  pkgs.coreutils
                  pkgs.gnused
                ];
              }
              ''
                mkdir -p work/bin work/var
                cat > work/bin/validation-stub <<'EOF'
                #!${pkgs.runtimeShell}
                set -eu
                if [ "''${1-}" = --artifact-identity ]; then
                  ${pkgs.coreutils}/bin/cat "$PWD/work/var/artifact-identity.json"
                  exit 0
                fi
                transcript=$PWD/transcript
                printf 'ARGV' > "$transcript"
                printf ' <%s>' "$@" >> "$transcript"
                printf '\n' >> "$transcript"
                ${pkgs.coreutils}/bin/env | ${pkgs.coreutils}/bin/sort >> "$transcript"
                regulatory_snapshot=$(${pkgs.coreutils}/bin/cat <&4)
                if [ "''${1-}" = --run-one-shot-sae-auth ] || [ "''${1-}" = --full-firmware-preflight ]; then
                  credential=$(${pkgs.coreutils}/bin/cat <&3)
                  printf 'CREDENTIAL_LEN=%s\n' "''${#credential}" >> "$transcript"
                fi
                printf 'REGULATORY_SNAPSHOT=%s\n' "$regulatory_snapshot" >> "$transcript"
                EOF
                chmod 0755 work/bin/validation-stub
                cat > work/bin/snapshot-stub <<'EOF'
                #!${pkgs.runtimeShell}
                set -eu
                test "$1" = --generate-regulatory-snapshot-v20
                test "$3" = 00
                printf snapshot-ok
                EOF
                chmod 0755 work/bin/snapshot-stub
                : > work/var/regulatory.db
                printf 'Passphrase=eight-by\n' > work/var/ph1.psk
                cp work/var/ph1.psk work/var/mock-ph1.psk
                printf '%s\n' '{"artifact_identity":"mt7921-validation-v1","flavor":"full-firmware-production","enabled_operation":"run-one-shot-sae-auth","source_commit":"launcher-test","fd_contract":"credential-fd3+snapshot-fd4+immediate-eof","active_capable":true}' > work/var/artifact-identity.json
                substitute ${./nix/mt7921-full-firmware-validation-launcher.sh} work/launcher \
                  --subst-var-by shell ${pkgs.runtimeShell} \
                  --subst-var-by driver "$PWD/work/bin/validation-stub" \
                  --subst-var-by snapshot_generator "$PWD/work/bin/snapshot-stub" \
                  --subst-var-by regulatory_db "$PWD/work/var/regulatory.db" \
                  --subst-var-by regulatory_source_sha256 0000000000000000000000000000000000000000000000000000000000000000 \
                  --subst-var-by credential_file "$PWD/work/var/ph1.psk" \
                  --subst-var-by mock_credential_file "$PWD/work/var/mock-ph1.psk" \
                  --subst-var-by artifact_identity "$PWD/work/var/artifact-identity.json" \
                  --subst-var-by cat ${pkgs.coreutils}/bin/cat \
                  --subst-var-by sed ${pkgs.gnused}/bin/sed \
                  --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                  --subst-var-by wc ${pkgs.coreutils}/bin/wc \
                  --subst-var-by stat ${pkgs.coreutils}/bin/stat \
                  --subst-var-by rm ${pkgs.coreutils}/bin/rm \
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
                grep -Fx 'DRV_REGULATORY_SNAPSHOT_FD=4' transcript
                grep -Fx 'DRV_REGULATORY_SNAPSHOT_LEN=11' transcript
                grep -Fx 'DRV_REGULATORY_SOURCE_SHA256=0000000000000000000000000000000000000000000000000000000000000000' transcript
                grep -Fx 'CREDENTIAL_LEN=8' transcript
                grep -Fx 'REGULATORY_SNAPSHOT=snapshot-ok' transcript
                ! grep -q 'PATCH_TABLE' transcript
                ! grep -q 'EAPOL' transcript
                rm transcript
                work/launcher --full-firmware-preflight
                grep -Fx 'ARGV <--full-firmware-preflight>' transcript
                grep -Fx 'DRV_SAE_CREDENTIAL_FD=3' transcript
                grep -Fx 'DRV_SAE_CREDENTIAL_LEN=8' transcript
                grep -Fx 'DRV_REGULATORY_SNAPSHOT_FD=4' transcript
                grep -Fx 'DRV_REGULATORY_SNAPSHOT_LEN=11' transcript
                grep -Fx 'REGULATORY_SNAPSHOT=snapshot-ok' transcript
                grep -Fx 'CREDENTIAL_LEN=8' transcript
                cp transcript "$out"
              '';

          mt7921-rate-power-evidence-launcher-test =
            pkgs.runCommand "mt7921-rate-power-evidence-launcher-test"
              {
                nativeBuildInputs = [ pkgs.coreutils pkgs.gnused ];
              }
              ''
                mkdir -p work/bin work/var
                cat > work/bin/evidence-stub <<'EOF'
                #!${pkgs.runtimeShell}
                set -eu
                if [ "''${1-}" = --artifact-identity ]; then
                  ${pkgs.coreutils}/bin/cat "$PWD/work/var/artifact-identity.json"
                  exit 0
                fi
                test -f "$PWD/generation.complete"
                transcript=$PWD/transcript
                printf 'ARGV <%s>\n' "$1" > "$transcript"
                ${pkgs.coreutils}/bin/env | ${pkgs.coreutils}/bin/sort >> "$transcript"
                credential=$(${pkgs.coreutils}/bin/cat <&3)
                snapshot=$(${pkgs.coreutils}/bin/cat <&4)
                if printf x >&4 2>/dev/null; then
                  echo FD4_WRITABLE >> "$transcript"
                  exit 1
                fi
                printf 'CREDENTIAL=%s\nSNAPSHOT=%s\nFD4_EOF=true\n' \
                  "$credential" "$snapshot" >> "$transcript"
                EOF
                chmod 0755 work/bin/evidence-stub
                cat > work/bin/snapshot-stub <<'EOF'
                #!${pkgs.runtimeShell}
                set -eu
                test "$1" = --generate-regulatory-snapshot-v20
                test "$3" = 00
                test "$4" = 0000000000000000000000000000000000000000000000000000000000000000
                : > "$PWD/generation.complete"
                printf snapshot-ok
                EOF
                chmod 0755 work/bin/snapshot-stub
                : > work/var/regulatory.db
                printf 'Passphrase=eight-by\n' > work/var/ph1.psk
                printf '%s\n' '{"artifact_identity":"mt7921-validation-v1","flavor":"rate-power-evidence-only","enabled_operation":"run-one-shot-power-setup","source_commit":"launcher-test","fd_contract":"credential-fd3+snapshot-fd4+immediate-eof","active_capable":false}' > work/var/artifact-identity.json
                substitute ${./nix/mt7921-rate-power-evidence-launcher.sh} work/launcher \
                  --subst-var-by shell ${pkgs.runtimeShell} \
                  --subst-var-by driver "$PWD/work/bin/evidence-stub" \
                  --subst-var-by snapshot_generator "$PWD/work/bin/snapshot-stub" \
                  --subst-var-by regulatory_db "$PWD/work/var/regulatory.db" \
                  --subst-var-by regulatory_source_sha256 0000000000000000000000000000000000000000000000000000000000000000 \
                  --subst-var-by credential_file "$PWD/work/var/ph1.psk" \
                  --subst-var-by artifact_identity "$PWD/work/var/artifact-identity.json" \
                  --subst-var-by cat ${pkgs.coreutils}/bin/cat \
                  --subst-var-by sed ${pkgs.gnused}/bin/sed \
                  --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                  --subst-var-by wc ${pkgs.coreutils}/bin/wc \
                  --subst-var-by stat ${pkgs.coreutils}/bin/stat \
                  --subst-var-by rm ${pkgs.coreutils}/bin/rm \
                  --subst-var-by env ${pkgs.coreutils}/bin/env
                chmod 0755 work/launcher
                env -i \
                  DRV_PCI_BDF=0000:05:00.0 DRV_IOMMU_GROUP=17 \
                  DRV_VFIO_DEVICE=/dev/vfio/devices/vfio17 \
                  DRV_LAB_SAFETY_STATE=/run/wifi-driver-lab/fixed.state.safety \
                  work/launcher
                grep -Fx 'ARGV <--run-one-shot-power-setup>' transcript
                grep -Fx 'DRV_SAE_CREDENTIAL_FD=3' transcript
                grep -Fx 'DRV_REGULATORY_SNAPSHOT_FD=4' transcript
                grep -Fx 'DRV_REGULATORY_SNAPSHOT_LEN=11' transcript
                grep -Fx 'CREDENTIAL=eight-by' transcript
                grep -Fx 'SNAPSHOT=snapshot-ok' transcript
                grep -Fx 'FD4_EOF=true' transcript
                ! grep -q FD4_WRITABLE transcript

                work/launcher --evidence-preflight
                grep -Fx 'ARGV <--full-firmware-preflight>' transcript
                grep -Fx 'DRV_SAE_CREDENTIAL_FD=3' transcript
                grep -Fx 'DRV_SAE_CREDENTIAL_LEN=8' transcript
                grep -Fx 'CREDENTIAL=eight-by' transcript
                grep -Fx 'SNAPSHOT=snapshot-ok' transcript

                cat > work/bin/fail-stub <<'EOF'
                #!${pkgs.runtimeShell}
                exit 1
                EOF
                chmod 0755 work/bin/fail-stub
                ${pkgs.gnused}/bin/sed \
                  "s|snapshot_generator=$PWD/work/bin/snapshot-stub|snapshot_generator=$PWD/work/bin/fail-stub|" \
                  work/launcher > work/fail-launcher
                chmod 0755 work/fail-launcher
                rm -f transcript
                if env -i \
                  DRV_PCI_BDF=0000:05:00.0 DRV_IOMMU_GROUP=17 \
                  DRV_VFIO_DEVICE=/dev/vfio/devices/vfio17 \
                  DRV_LAB_SAFETY_STATE=/run/wifi-driver-lab/fixed.state.safety \
                  work/fail-launcher; then
                  exit 1
                fi
                test ! -e transcript
                printf passed > "$out"
              '';

          mt7921-rate-power-evidence-supervisor = pkgs.runCommand
            "mt7921-rate-power-evidence-supervisor"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils ]; }
            ''
              mkdir -p "$out/bin"
              substitute ${./crates/mt7921-port-spike/lab/selector-write-recovery-supervisor.sh} \
                "$out/bin/mt7921-rate-power-evidence-supervisor" \
                --subst-var-by runtime_path /run/current-system/sw/bin \
                --subst-var-by wifi_driver_lab /run/current-system/sw/bin/wifi-driver-lab \
                --subst-var-by wifi_lab_watchdog /run/current-system/sw/bin/wifi-lab-watchdog \
                --subst-var-by validation_launcher ${mt7921-rate-power-evidence}/bin/mt7921-rate-power-evidence \
                --subst-var-by artifact_identity ${mt7921-rate-power-evidence}/share/mt7921-rate-power-evidence/artifact-identity.json \
                --subst-var-by recovery_samples 45 \
                --subst-var-by sys_root /sys \
                --subst-var-by run_root /run \
                --subst-var-by var_root /var \
                --subst-var-by id_command ${pkgs.coreutils}/bin/id
              chmod 0755 "$out/bin/mt7921-rate-power-evidence-supervisor"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-rate-power-evidence-supervisor"
              grep -F '${mt7921-rate-power-evidence}/bin/mt7921-rate-power-evidence' \
                "$out/bin/mt7921-rate-power-evidence-supervisor"
            '';

          mt7921-rate-power-evidence-manifest =
            let
              package = mt7921-rate-power-evidence;
              supervisor = mt7921-rate-power-evidence-supervisor;
              closure = pkgs.closureInfo { rootPaths = [ package supervisor ]; };
            in
            pkgs.runCommand "mt7921-rate-power-evidence-manifest"
              { nativeBuildInputs = [ pkgs.coreutils ]; }
              ''
                launcher=${package}/bin/mt7921-rate-power-evidence
                elf=${package}/libexec/mt7921-rate-power-evidence
                supervisor=${supervisor}/bin/mt7921-rate-power-evidence-supervisor
                identity=${package}/share/mt7921-rate-power-evidence/artifact-identity.json
                cat > "$out" <<EOF
                PACKAGE=${package}
                LAUNCHER=$launcher
                LAUNCHER_SHA256=$(sha256sum "$launcher" | cut -d ' ' -f1)
                ELF=$elf
                ELF_SHA256=$(sha256sum "$elf" | cut -d ' ' -f1)
                ARTIFACT_IDENTITY=$identity
                ARTIFACT_IDENTITY_SHA256=$(sha256sum "$identity" | cut -d ' ' -f1)
                FLAVOR=rate-power-evidence-only
                ACTIVE_CAPABLE=false
                FD_CONTRACT=credential-fd3+snapshot-fd4+immediate-eof
                SUPERVISOR=$supervisor
                SUPERVISOR_SHA256=$(sha256sum "$supervisor" | cut -d ' ' -f1)
                REGULATORY_DB=${regulatoryDb}
                REGULATORY_DB_SHA256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
                CLOSURE_SHA256=$(sort ${closure}/store-paths | sha256sum | cut -d ' ' -f1)
                PCI_BDF=0000:05:00.0
                TIMEOUT_SECONDS=300
                OPERATION=--run-one-shot-power-setup
                ORDER=PATCH_RAM_RX_PATH_8x0x4005d_ACKED_ADD_DEVICE_STOP
                NATIVE_RAW_SHA256=a518536c96d2de1a8ba398cbd0fbdb1b00f5b90434d27e8de2fefb97b2ba7b95,1c365518ffebb7a2b12b924bcfbe41436b4f2d52a83b07682d6f16c79eb39db0,1cbc40088bc367d75b2119806d3a8114ed3c4ce92a9572f262faace837343d36,f03e59182fd1e605df82de38305d445a015c9fc44c12da7802132514fed3e4d5,231e8db12ba160bdc12af55ef61c2da091387951c029008cc68b6b8d8a71d208,d8761b04f27de826280c55aac39a96e4d3bc0bb4d83330d910a4fd3ca70b90b2,e018434f160c1b67dc86477c602a87627b5553c5304359912347a04cbb3aad44,f1e5d489bb579d4d080eb8c2569e752c0b0dd87ccbdf02a112705536791d89df
                NORMALIZED_ENVELOPE_SHA256=72ea1befa9b1ff66bd677d92bde13d3d5e89409abc5afdd2e9272d65a5ddb7c7,20357281549ddaa5598b30fbbde185dedfd345ebfcef147741c41fccb3a5622e,b003e1b0460ec05444eda58cb6ba87ce2753e84b0bfd81c2102cbb62f15db735,3162681f7c44963e53b0739c59426677326490c1e70d6736ec6e317ca4e7a389,9d394dfd9b0e45eca67741a1aaca4631280b5e238ed82c0858e2942155c3ae85,28828e4df8b780205d7784a88c2e51883a379d8b3cc71cac5ff7bff2d0396bbf,0a33bb15880033bc175c79962ef1c1021da3067be28a3bbf29a7d3ae40414596,997ae34108569cb25783d44bac88e3631920188a7ec1c278bda69686c19db873
                INERT_ARTIFACTS=${package}/share/mt7921-rate-power-evidence
                TMAC_POPULATION_INVARIANT=false
                SCAN=false
                CHANNEL_SET=false
                AUTHENTICATION=false
                FRAME_TX=false
                PRIVILEGE_CONTRACT=FIXED_ROOT_ENTRY_SUDO_-n
                EOF
              '';

          mt7921-rate-power-evidence-root-entry = pkgs.runCommand
            "mt7921-rate-power-evidence-root-entry"
            { nativeBuildInputs = [ pkgs.coreutils ]; }
            ''
              mkdir -p "$out/bin"
              substitute ${./nix/mt7921-rate-power-evidence-semantic-root.sh} \
                "$out/bin/mt7921-rate-power-evidence-root" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by sudo /run/wrappers/bin/sudo \
                --subst-var-by supervisor ${mt7921-rate-power-evidence-supervisor}/bin/mt7921-rate-power-evidence-supervisor \
                --subst-var-by launcher ${mt7921-rate-power-evidence}/bin/mt7921-rate-power-evidence \
                --subst-var-by manifest ${mt7921-rate-power-evidence-manifest} \
                --subst-var-by artifact_identity ${mt7921-rate-power-evidence}/share/mt7921-rate-power-evidence/artifact-identity.json \
                --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                --subst-var-by cut ${pkgs.coreutils}/bin/cut
              chmod 0755 "$out/bin/mt7921-rate-power-evidence-root"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-rate-power-evidence-root"
            '';

          mt7921-rate-power-evidence-privilege-test = pkgs.runCommand
            "mt7921-rate-power-evidence-privilege-test"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils ]; }
            ''
              mkdir -p work/bin work/var/lib/wifi-driver-lab
              cat > work/bin/launcher-stub <<'EOF'
              #!${pkgs.runtimeShell}
              if [ "''${1-}" = --artifact-identity ]; then cat "$PWD/work/artifact-identity.json"; exit 0; fi
              exit 0
              EOF
              echo '{"artifact_identity":"test"}' > work/artifact-identity.json
              chmod 0755 work/bin/launcher-stub
              launcher=$PWD/work/bin/launcher-stub
              cat > work/bin/id-unprivileged <<'EOF'
              #!${pkgs.runtimeShell}
              echo 1000
              EOF
              cat > work/bin/id-root <<'EOF'
              #!${pkgs.runtimeShell}
              echo 0
              EOF
              cat > work/bin/hardware-stub <<'EOF'
              #!${pkgs.runtimeShell}
              echo hardware-called >> "$PWD/transcript"
              exit 1
              EOF
              chmod 0755 work/bin/*
              make_supervisor() {
                local id=$1 output=$2
                substitute ${./crates/mt7921-port-spike/lab/selector-write-recovery-supervisor.sh} "$output" \
                  --subst-var-by runtime_path ${pkgs.coreutils}/bin \
                  --subst-var-by wifi_driver_lab "$PWD/work/bin/hardware-stub" \
                  --subst-var-by wifi_lab_watchdog "$PWD/work/bin/hardware-stub" \
                  --subst-var-by validation_launcher "$launcher" \
                  --subst-var-by artifact_identity "$PWD/work/artifact-identity.json" \
                  --subst-var-by recovery_samples 1 \
                  --subst-var-by sys_root "$PWD/work/sys" \
                  --subst-var-by run_root "$PWD/work/run" \
                  --subst-var-by var_root "$PWD/work/var" \
                  --subst-var-by id_command "$id"
                chmod 0755 "$output"
              }
              make_supervisor "$PWD/work/bin/id-unprivileged" work/supervisor-unprivileged
              if ${pkgs.bash}/bin/bash work/supervisor-unprivileged 0000:05:00.0 -- "$launcher" 2>error; then exit 1; fi
              grep -F 'requires noninteractive root elevation before any state change' error
              test ! -e transcript
              test -z "$(find work/var/lib/wifi-driver-lab -type f -print -quit)"

              make_supervisor "$PWD/work/bin/id-root" work/supervisor-root
              ${pkgs.bash}/bin/bash work/supervisor-root --plan 0000:05:00.0 -- "$launcher" > plan
              grep -F 'mode=inert hardware_handoff=false uid=0 privilege_contract=sudo_-n' plan
              grep -F 'durable_report_writable=true' plan
              test ! -e transcript

              : > work/manifest
              cat > work/bin/sudo-stub <<'EOF'
              #!${pkgs.runtimeShell}
              printf '%s\n' "$@" > "$PWD/sudo.argv"
              exit 0
              EOF
              chmod 0755 work/bin/sudo-stub
              substitute ${./nix/mt7921-rate-power-evidence-root.sh} work/root-entry \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by sudo "$PWD/work/bin/sudo-stub" \
                --subst-var-by supervisor "$PWD/work/supervisor-root" \
                --subst-var-by launcher "$launcher" \
                --subst-var-by manifest "$PWD/work/manifest" \
                --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                --subst-var-by cut ${pkgs.coreutils}/bin/cut
              chmod 0755 work/root-entry
              work/root-entry
              printf '%s\n' -n "$PWD/work/supervisor-root" 0000:05:00.0 -- "$launcher" > expected
              cmp expected sudo.argv
              work/root-entry --plan
              printf '%s\n' -n "$PWD/work/supervisor-root" --plan 0000:05:00.0 -- "$launcher" > expected
              cmp expected sudo.argv
              if work/root-entry arbitrary 2>error; then exit 1; fi
              grep -F 'accepts no arguments except --plan' error
              printf passed > "$out"
            '';

          mt7921-full-firmware-validation-supervisor = pkgs.runCommand
            "mt7921-full-firmware-validation-supervisor"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils ]; }
            ''
              mkdir -p "$out/bin"
              substitute ${./crates/mt7921-port-spike/lab/selector-write-recovery-supervisor.sh} \
                "$out/bin/mt7921-full-firmware-validation-supervisor" \
                --subst-var-by runtime_path /run/current-system/sw/bin \
                --subst-var-by wifi_driver_lab /run/current-system/sw/bin/wifi-driver-lab \
                --subst-var-by wifi_lab_watchdog /run/current-system/sw/bin/wifi-lab-watchdog \
                --subst-var-by validation_launcher ${mt7921-full-firmware-validation}/bin/mt7921-full-firmware-validation \
                --subst-var-by artifact_identity ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json \
                --subst-var-by recovery_samples 45 \
                --subst-var-by sys_root /sys \
                --subst-var-by run_root /run \
                --subst-var-by var_root /var \
                --subst-var-by id_command ${pkgs.coreutils}/bin/id
              chmod 0755 "$out/bin/mt7921-full-firmware-validation-supervisor"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-validation-supervisor"
              grep -F 'connected Wi-Fi target drifted from fixed ph1 validation policy' \
                "$out/bin/mt7921-full-firmware-validation-supervisor"
            '';

          mt7921-full-firmware-validation-manifest =
            let
              package = mt7921-full-firmware-validation;
              supervisor = mt7921-full-firmware-validation-supervisor;
              closure = pkgs.closureInfo { rootPaths = [ package supervisor ]; };
            in
            pkgs.runCommand "mt7921-full-firmware-validation-manifest"
              { nativeBuildInputs = [ pkgs.coreutils ]; }
              ''
                launcher=${package}/bin/mt7921-full-firmware-validation
                driver=${package}/bin/mt7921-full-firmware-validation-driver
                supervisor=${supervisor}/bin/mt7921-full-firmware-validation-supervisor
                identity=${package}/share/mt7921-full-firmware-validation/artifact-identity.json
                closure_sha=$(sort ${closure}/store-paths | sha256sum | cut -d ' ' -f1)
                cat > "$out" <<EOF
                PACKAGE=${package}
                LAUNCHER=$launcher
                LAUNCHER_SHA256=$(sha256sum "$launcher" | cut -d ' ' -f1)
                ELF=$driver
                ELF_SHA256=$(sha256sum "$driver" | cut -d ' ' -f1)
                ARTIFACT_IDENTITY=$identity
                ARTIFACT_IDENTITY_SHA256=$(sha256sum "$identity" | cut -d ' ' -f1)
                FLAVOR=full-firmware-production
                ACTIVE_CAPABLE=true
                FD_CONTRACT=credential-fd3+snapshot-fd4+immediate-eof
                SUPERVISOR=$supervisor
                SUPERVISOR_SHA256=$(sha256sum "$supervisor" | cut -d ' ' -f1)
                REGULATORY_DB=${regulatoryDb}
                REGULATORY_DB_SHA256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
                REGULATORY_GENERATION=0
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
                TMAC_DIAGNOSTIC_ONLY=true
                TMAC_POPULATION_INVARIANT=false
                RATE_POWER_ORDER=eeprom_prepare_protect_mac_enable_rx_path_then_8_contiguous_0x4005d_then_acked_add_device
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

          mt7921-full-firmware-validation-root-entry = pkgs.runCommand
            "mt7921-full-firmware-validation-root-entry"
            {
              nativeBuildInputs = [ pkgs.bash pkgs.coreutils ];
              meta.mainProgram = "mt7921-full-firmware-validation-root";
            }
            ''
              mkdir -p "$out/bin"
              substitute ${./nix/mt7921-full-firmware-validation-root.sh} \
                "$out/bin/mt7921-full-firmware-validation-root" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by sudo /run/wrappers/bin/sudo \
                --subst-var-by supervisor ${mt7921-full-firmware-validation-supervisor}/bin/mt7921-full-firmware-validation-supervisor \
                --subst-var-by launcher ${mt7921-full-firmware-validation}/bin/mt7921-full-firmware-validation \
                --subst-var-by manifest ${mt7921-full-firmware-validation-manifest} \
                --subst-var-by artifact_identity ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json \
                --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                --subst-var-by cut ${pkgs.coreutils}/bin/cut
              chmod 0755 "$out/bin/mt7921-full-firmware-validation-root"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-validation-root"
            '';

          mt7921-validation-flavor-cross-wire-test = pkgs.runCommand
            "mt7921-validation-flavor-cross-wire-test"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils ]; }
            ''
              cat > sudo-stub <<'EOF'
              #!${pkgs.runtimeShell}
              echo called > "$PWD/sudo-called"
              exit 1
              EOF
              chmod +x sudo-stub
              make_root() {
                template=$1 output=$2 launcher=$3 manifest=$4 identity=$5
                substitute "$template" "$output" \
                  --subst-var-by shell ${pkgs.runtimeShell} \
                  --subst-var-by sudo "$PWD/sudo-stub" \
                  --subst-var-by supervisor ${pkgs.coreutils}/bin/false \
                  --subst-var-by launcher "$launcher" \
                  --subst-var-by manifest "$manifest" \
                  --subst-var-by artifact_identity "$identity" \
                  --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                  --subst-var-by cut ${pkgs.coreutils}/bin/cut
                chmod +x "$output"
              }
              production_launcher=${mt7921-full-firmware-validation}/bin/mt7921-full-firmware-validation
              evidence_launcher=${mt7921-rate-power-evidence}/bin/mt7921-rate-power-evidence
              production_identity=${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json
              evidence_identity=${mt7921-rate-power-evidence}/share/mt7921-rate-power-evidence/artifact-identity.json
              test "$("$production_launcher" --artifact-identity)" = "$(cat "$production_identity")"
              test "$("$evidence_launcher" --artifact-identity)" = "$(cat "$evidence_identity")"
              test "$(cat "$production_identity")" != "$(cat "$evidence_identity")"

              make_root ${./nix/mt7921-full-firmware-validation-root.sh} prod-wrong-elf \
                "$evidence_launcher" ${mt7921-full-firmware-validation-manifest} "$production_identity"
              make_root ${./nix/mt7921-full-firmware-validation-root.sh} prod-wrong-manifest \
                "$production_launcher" ${mt7921-rate-power-evidence-manifest} "$production_identity"
              make_root ${./nix/mt7921-rate-power-evidence-semantic-root.sh} evidence-wrong-elf \
                "$production_launcher" ${mt7921-rate-power-evidence-manifest} "$evidence_identity"
              make_root ${./nix/mt7921-rate-power-evidence-semantic-root.sh} evidence-wrong-manifest \
                "$evidence_launcher" ${mt7921-full-firmware-validation-manifest} "$evidence_identity"
              for root in prod-wrong-elf prod-wrong-manifest evidence-wrong-elf evidence-wrong-manifest; do
                if ./$root --plan; then
                  echo "cross-wired root unexpectedly passed: $root" >&2
                  exit 1
                fi
              done
              test ! -e sudo-called
              touch "$out"
            '';

          mt7921-full-firmware-inert-proof =
            let
              closureRoots = [
                mt7921-full-firmware-validation
                mt7921-full-firmware-validation-supervisor
                mt7921-full-firmware-validation-manifest
                mt7921-full-firmware-validation-root-entry
                pkgs.runtimeShell
                pkgs.coreutils
                pkgs.diffutils
                pkgs.gnused
                pkgs.nettools
                pkgs.nix
              ];
              closure = pkgs.runCommand "mt7921-full-firmware-inert-proof-closure-metadata"
                {
                  __structuredAttrs = true;
                  exportReferencesGraph.closure = closureRoots;
                  nativeBuildInputs = [ pkgs.jq ];
                }
                ''
                  out="''${outputs[out]}"
                  mkdir -p "$out"
                  ${pkgs.jq}/bin/jq -er '
                    def valid_path: test("^/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[^/[:space:]\\t]+$");
                    def valid_hash: test("^sha256:[0123456789abcdfghijklmnpqrsvwxyz]{52}$");
                    .closure | sort_by(.path) as $rows
                    | if (($rows | length) == 75
                        and ($rows | map(.path) | unique | length) == 75
                        and all($rows[]; (.path | valid_path) and (.narHash | valid_hash)))
                      then $rows else error("invalid 75-path registered closure metadata") end
                    | .[] | [.path, .narHash] | @tsv
                  ' "$NIX_ATTRS_JSON_FILE" >"$out/closure.tsv"
                  cut -f1 "$out/closure.tsv" >"$out/closure.paths"
                  test "$(wc -l < "$out/closure.tsv")" -eq 75
                  test "$(awk -F '\t' 'NF == 2 && $1 != "" && $2 != "" { count++ } END { print count+0 }' "$out/closure.tsv")" -eq 75
                '';
              closureRootsFile = pkgs.writeText "mt7921-full-firmware-inert-proof-roots" (
                pkgs.lib.concatMapStringsSep "\n" toString closureRoots + "\n"
              );
            in
            pkgs.runCommand "mt7921-full-firmware-inert-proof"
              {
                nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.nix ];
                meta.mainProgram = "mt7921-full-firmware-inert-proof";
              }
              ''
                mkdir -p "$out/bin" "$out/libexec" "$out/share/mt7921-full-firmware-inert-proof"
                cp ${closure}/closure.paths ${closure}/closure.tsv "$out/share/mt7921-full-firmware-inert-proof/"
                cp ${closureRootsFile} "$out/share/mt7921-full-firmware-inert-proof/closure.roots"
                substitute ${./nix/mt7921-closure-manifest-generate.sh} manifest-generate \
                  --subst-var-by shell ${pkgs.runtimeShell}
                chmod 0755 manifest-generate
                substitute ${./nix/mt7921-closure-manifest-validate.sh} \
                  "$out/libexec/mt7921-closure-manifest-validate" \
                  --subst-var-by shell ${pkgs.runtimeShell} \
                  --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                  --subst-var-by sort ${pkgs.coreutils}/bin/sort
                chmod 0755 "$out/libexec/mt7921-closure-manifest-validate"
                substitute ${./nix/mt7921-closure-manifest-verify.sh} \
                  "$out/libexec/mt7921-closure-manifest-verify" \
                  --subst-var-by shell ${pkgs.runtimeShell} \
                  --subst-var-by validator "$out/libexec/mt7921-closure-manifest-validate" \
                  --subst-var-by sort ${pkgs.coreutils}/bin/sort \
                  --subst-var-by cmp ${pkgs.diffutils}/bin/cmp
                chmod 0755 "$out/libexec/mt7921-closure-manifest-verify"
                "$out/libexec/mt7921-closure-manifest-validate" \
                  "$out/share/mt7921-full-firmware-inert-proof/closure.tsv" \
                  "$out/share/mt7921-full-firmware-inert-proof/closure.paths"
                test "$(wc -l < "$out/share/mt7921-full-firmware-inert-proof/closure.tsv")" -eq 75
                closure_manifest_sha256="$(${pkgs.coreutils}/bin/sha256sum "$out/share/mt7921-full-firmware-inert-proof/closure.tsv" | cut -d' ' -f1)"
                substitute ${./nix/mt7921-full-firmware-inert-proof.sh} \
                  "$out/bin/mt7921-full-firmware-inert-proof" \
                  --subst-var-by shell ${pkgs.runtimeShell} \
                  --subst-var-by package ${mt7921-full-firmware-validation} \
                  --subst-var-by supervisor ${mt7921-full-firmware-validation-supervisor} \
                  --subst-var-by manifest ${mt7921-full-firmware-validation-manifest} \
                  --subst-var-by root_entry ${mt7921-full-firmware-validation-root-entry} \
                  --subst-var-by expected_hashes "$out/share/mt7921-full-firmware-inert-proof/closure.tsv" \
                  --subst-var-by closure_roots "$out/share/mt7921-full-firmware-inert-proof/closure.roots" \
                  --subst-var-by closure_manifest_sha256 "$closure_manifest_sha256" \
                  --subst-var-by manifest_verifier "$out/libexec/mt7921-closure-manifest-verify" \
                  --subst-var-by commit aefc95ec3adea38d7ffbac4475cfbcb2848a9f38 \
                  --subst-var-by id ${pkgs.coreutils}/bin/id \
                  --subst-var-by date ${pkgs.coreutils}/bin/date \
                  --subst-var-by install ${pkgs.coreutils}/bin/install \
                  --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                  --subst-var-by hostname ${pkgs.nettools}/bin/hostname \
                  --subst-var-by sed ${pkgs.gnused}/bin/sed \
                  --subst-var-by nix_store ${pkgs.nix}/bin/nix-store \
                  --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                  --subst-var-by cut ${pkgs.coreutils}/bin/cut \
                  --subst-var-by diff ${pkgs.diffutils}/bin/diff
                chmod 0755 "$out/bin/mt7921-full-firmware-inert-proof"
                ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-inert-proof"
                ${pkgs.bash}/bin/bash -n "$out/libexec/mt7921-closure-manifest-validate"
                ${pkgs.bash}/bin/bash -n "$out/libexec/mt7921-closure-manifest-verify"

                # Hermetic regression coverage for the blank-column failure and
                # every structural/hash-set rejection made by the target runner.
                cp "$out/share/mt7921-full-firmware-inert-proof/closure.tsv" valid.tsv
                cp "$out/share/mt7921-full-firmware-inert-proof/closure.paths" valid.paths
                "$out/libexec/mt7921-closure-manifest-validate" valid.tsv valid.paths
                printf '#!%s\nexit 1\n' ${pkgs.runtimeShell} > hash-fails
                printf '#!%s\nexit 0\n' ${pkgs.runtimeShell} > hash-empty
                chmod +x hash-fails hash-empty
                if ./manifest-generate generated.tsv valid.paths -- ./hash-fails; then exit 1; fi
                if ./manifest-generate generated.tsv valid.paths -- ./hash-empty; then exit 1; fi
                sed '1s/\t.*$/\t/' valid.tsv > blank.tsv
                sed '1s/sha256:.*/sha256:not-a-hash/' valid.tsv > malformed.tsv
                sed '1s/$/\textra/' valid.tsv > three-field.tsv
                head -c -1 valid.tsv > unterminated.tsv
                { cat valid.tsv; head -1 valid.tsv; } > duplicate.tsv
                tail -n +2 valid.tsv > missing.tsv
                { cat valid.tsv; printf '/nix/store/00000000000000000000000000000000-extra\tsha256:0000000000000000000000000000000000000000000000000000\n'; } > extra.tsv
                ${pkgs.gawk}/bin/awk -F '\t' 'BEGIN { OFS="\t" } NR == 1 { c=substr($2,8,1); r=(c=="0"?"1":"0"); $2=substr($2,1,7) r substr($2,9) } { print }' \
                  valid.tsv > wrong.tsv
                cat > store-stub <<EOF
                #!${pkgs.runtimeShell}
                set -euo pipefail
                mode=\$(basename "\$0")
                case "\$1" in
                  -qR) cat "$PWD/stub.actual" ;;
                  --verify-path) test "\$mode" != store-verify-fails ;;
                  -q)
                    test "\$2" = --hash
                    case "\$mode" in
                      store-fails) exit 1 ;;
                      store-empty) exit 0 ;;
                    esac
                    ${pkgs.gawk}/bin/awk -F '\t' -v path="\$3" '\$1 == path { print \$2; found=1 } END { exit !found }' "$PWD/valid.tsv"
                    ;;
                  *) exit 64 ;;
                esac
                EOF
                chmod +x store-stub
                for mode in store-fails store-empty store-verify-fails; do ln -s store-stub "$mode"; done
                cp valid.paths stub.actual
                mkdir verify-output
                verify="$out/libexec/mt7921-closure-manifest-verify"
                "$verify" valid.tsv "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-stub"
                for bad in blank malformed three-field unterminated duplicate missing extra wrong; do
                  if "$verify" "$bad.tsv" "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-stub"; then exit 1; fi
                done
                if "$verify" valid.tsv "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-fails"; then exit 1; fi
                if "$verify" valid.tsv "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-empty"; then exit 1; fi
                if "$verify" valid.tsv "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-verify-fails"; then exit 1; fi
                tail -n +2 valid.paths > stub.actual
                if "$verify" valid.tsv "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-stub"; then exit 1; fi
                { cat valid.paths; echo /nix/store/00000000000000000000000000000000-extra; } > stub.actual
                if "$verify" valid.tsv "$out/share/mt7921-full-firmware-inert-proof/closure.roots" verify-output "$PWD/store-stub"; then exit 1; fi
                "$out/bin/mt7921-full-firmware-inert-proof" --plan > plan
                grep -F 'hardware_handoff=false active_validation=false' plan
                grep -F 'canonical_fd3_fd4=true' plan
                grep -F 'trap_safe=true' plan
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
