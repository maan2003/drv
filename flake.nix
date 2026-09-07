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
          mt7921FuchsiaSource = pkgs.callPackage ./nix/mt7921-fuchsia-source.nix { };
        in
        rec {
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

              association_verify=${./nix/verify-mt7921-production-association-path.sh}
              adapter_source=${./crates/mt7921-softmac-adapter/src/client_device.rs}
              binary_source=${./crates/mt7921-port-spike/src/bin/vfio_read.rs}
              fixture_source=${./crates/netstack3-port-spike/upstream-cargo/src/connectivity/wlan/lib/mlme/rust/src/host_fixture.rs}
              bash "$association_verify" "$adapter_source" "$binary_source" "$fixture_source"
              cp "$adapter_source" stale-adapter.rs
              cp "$fixture_source" swapped-oracle-fixture.rs
              sed -i 's/prepare_production_wlan_frame(/stale_fixture_only_profile(/' stale-adapter.rs
              sed -i 's/linux_61840_oracle_profile()/AssociationRequestProfile::default()/' swapped-oracle-fixture.rs
              ! bash "$association_verify" stale-adapter.rs "$binary_source" "$fixture_source"
              ! bash "$association_verify" "$adapter_source" "$binary_source" swapped-oracle-fixture.rs

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
                --subst-var-by credential_file /var/lib/iwd/ajay.psk \
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
              strings "$driver" | grep -F 'rate_power_conformance=true'
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

          mt7921-full-firmware-validation =
            assert
              let spoof = builtins.getEnv "MT7921_FULL_FIRMWARE_SOURCE_COMMIT";
              in spoof == "" || throw "MT7921_FULL_FIRMWARE_SOURCE_COMMIT is rejected: production identity is derived from immutable source hashes";
            pkgs.rustPlatform.buildRustPackage {
            pname = "mt7921-full-firmware-validation";
            version = "0.1.0";
            src = mt7921FuchsiaSource;
            cargoRoot = "crates/mt7921-passive-scan";
            buildAndTestSubdir = "crates/mt7921-passive-scan";
            cargoLock.lockFile = ./crates/mt7921-passive-scan/Cargo.lock;
            cargoBuildFlags = [
              "--no-default-features"
              "--features"
              "fuchsia-passive,full-firmware-production"
            ];
            nativeBuildInputs = [ pkgs.cmake pkgs.pkg-config pkgs.perl ];
            MT7921_FUCHSIA_BASE_REVISION = mt7921FuchsiaSource.fuchsiaBaseRevision;
            MT7921_FUCHSIA_ORDERED_PATCH_SET_SHA256 = mt7921FuchsiaSource.fuchsiaOrderedPatchSetSha256;
            MT7921_FUCHSIA_ORDERED_PATCH_LIST = mt7921FuchsiaSource.fuchsiaOrderedPatchList;
            session_client_mac = "8a:fd:2a:8b:70:5a";
            preBuild = ''
              ref=reference/fuchsia-${mt7921FuchsiaSource.fuchsiaBaseRevision}
              export MT7921_MATERIALIZED_SOURCE_TREE_SHA256=$(cat "$ref/.drv-materialized-source-tree-sha256")
              export MT7921_GENERATED_CRATE_SOURCE_SHA256=$(cat "$ref/.drv-generated-crate-source-sha256")
              export MT7921_SOURCE_IDENTITY_SHA256=$(cat "$ref/.drv-source-identity-sha256")
              export MT7921_PROJECT_CORE_SOURCE_SHA256=$(
                find crates/mt7921-core -type f -print0 | sort -z \
                  | while IFS= read -r -d $'\0' file; do printf '%s\0' "$file"; sha256sum "$file"; done \
                  | sha256sum | cut -d ' ' -f1
              )
              export MT7921_COMPOSITE_ARTIFACT_SOURCE_SHA256=$(
                printf '%s\n%s\n%s\n' "$MT7921_SOURCE_IDENTITY_SHA256" \
                  "$MT7921_PROJECT_CORE_SOURCE_SHA256" connac2-bss-wire-v1 \
                  | sha256sum | cut -d ' ' -f1
              )
              test "''${#MT7921_MATERIALIZED_SOURCE_TREE_SHA256}" -eq 64
              test "''${#MT7921_GENERATED_CRATE_SOURCE_SHA256}" -eq 64
              test "''${#MT7921_SOURCE_IDENTITY_SHA256}" -eq 64
              test "''${#MT7921_PROJECT_CORE_SOURCE_SHA256}" -eq 64
              test "''${#MT7921_COMPOSITE_ARTIFACT_SOURCE_SHA256}" -eq 64
            '';
            doCheck = false;
            installPhase = ''
              runHook preInstall
              driver=$(find target -path '*/release/mt7921-passive-scan' -type f -print -quit)
              test -n "$driver"
              install -Dm0755 "$driver" "$out/libexec/mt7921-full-firmware-validation"
              mkdir -p "$out/share/mt7921-full-firmware-validation"
              "$out/libexec/mt7921-full-firmware-validation" --artifact-identity \
                > "$out/share/mt7921-full-firmware-validation/artifact-identity.json"
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
                --subst-var-by credential_file /var/lib/iwd/ajay.psk \
                --subst-var-by artifact_identity "$out/share/mt7921-full-firmware-validation/artifact-identity.json" \
                --subst-var-by session_client_mac "$session_client_mac" \
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
              driver=$out/libexec/mt7921-full-firmware-validation
              "$driver" --artifact-identity > actual-identity.json
              cmp actual-identity.json "$evidence_dir/artifact-identity.json"
              "$driver" --self-test-rate-power-delivery > "$evidence_dir/rate-power-self-test.jsonl"
              cat > "$evidence_dir/ARTIFACTS" <<EOF
              RATE_POWER_SELF_TEST_SHA256=$(sha256sum "$evidence_dir/rate-power-self-test.jsonl" | cut -d ' ' -f1)
              FUCHSIA_BASE_REVISION=${mt7921FuchsiaSource.fuchsiaBaseRevision}
              FUCHSIA_ORDERED_PATCH_SET_SHA256=${mt7921FuchsiaSource.fuchsiaOrderedPatchSetSha256}
              PROJECT_CORE_SOURCE_SHA256=$MT7921_PROJECT_CORE_SOURCE_SHA256
              COMPOSITE_ARTIFACT_SOURCE_SHA256=$MT7921_COMPOSITE_ARTIFACT_SOURCE_SHA256
              BSS_WIRE_CONTRACT=connac2-bss-wire-v1
              REGULATORY_SOURCE_SHA256=$(sha256sum ${regulatoryDb} | cut -d ' ' -f1)
              REGULATORY_GENERATION=0
              EOF
            '';
            doInstallCheck = true;
            installCheckPhase = ''
              runHook preInstallCheck
              driver=$out/libexec/mt7921-full-firmware-validation
              identity=$out/share/mt7921-full-firmware-validation/artifact-identity.json
              test "$("$driver" --artifact-identity)" = "$(cat "$identity")"
              grep -F '"artifact_identity":"mt7921-driver-v11"' "$identity"
              grep -F '"flavor":"full-firmware-production"' "$identity"
              grep -F '"active_capable":true' "$identity"
              grep -F '"bss_wire_contract":"connac2-bss-wire-v1"' "$identity"
              grep -F '"initial_bss_command_sha256":"7aefeb7aa0e4eb196b676a1a5cb803cf287816abab430d6958021ffbf9cd273f"' "$identity"
              grep -F '"associated_bss_command_sha256":"6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5"' "$identity"
              rate_output="$("$driver" --self-test-rate-power-delivery)"
              test "$(printf '%s\n' "$rate_output" | grep -c '"rate_power_self_test":"command"')" -eq 10
              printf '%s\n' "$rate_output" | grep -F '"rate_power_self_test":"passed"'
              launcher=$out/bin/mt7921-full-firmware-validation
              grep -F 'DRV_ACTIVE_CLIENT=1' "$launcher"
              grep -F 'DRV_SAE_CLIENT_MAC=8a:fd:2a:8b:70:5a' "$launcher"
              grep -F -- '--run-one-shot-sae-auth' "$launcher"
              if "$launcher" --run-one-shot-patch-table-gate 2>/dev/null; then
                echo 'fixed launcher unexpectedly accepted patch-table gate dispatch' >&2
                exit 1
              fi
              runHook postInstallCheck
            '';
            meta.mainProgram = "mt7921-full-firmware-validation";
          };

          mt7921-fresh-laa-diagnostic =
            mt7921-full-firmware-validation.overrideAttrs (old: {
              pname = "mt7921-fresh-laa-diagnostic";
              session_client_mac = "02:7d:91:4c:b8:3e";
              cargoBuildFlags = [
                "--no-default-features"
                "--features"
                "fuchsia-passive,full-firmware-production,fresh-laa-diagnostic"
              ];
              installCheckPhase = ''
                runHook preInstallCheck
                driver=$out/libexec/mt7921-full-firmware-validation
                identity=$out/share/mt7921-full-firmware-validation/artifact-identity.json
                grep -F '"flavor":"fresh-laa-diagnostic"' "$identity"
                grep -F '"diagnostic_safety_class":"fixed-fresh-laa-stale-ap-state-attribution-only"' "$identity"
                grep -F '"session_identity_contract":"single-typed-source-fixed-fresh-laa-dev-muar-bss-omac-sme-mgmt-rx-v1"' "$identity"
                grep -F '"session_client_mac":"02:7d:91:4c:b8:3e"' "$identity"
                "$driver" --self-test-fresh-laa-identity | grep -F '"fresh_laa_identity_self_test":"passed"'
                grep -F 'DRV_SAE_CLIENT_MAC=02:7d:91:4c:b8:3e' "$out/bin/mt7921-full-firmware-validation"
                runHook postInstallCheck
              '';
            });

          mt7921-fresh-laa-diagnostic-supervisor = pkgs.runCommand
            "mt7921-fresh-laa-diagnostic-supervisor"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]; }
            ''
              mkdir -p "$out/bin"
              substitute ${./crates/mt7921-port-spike/lab/selector-write-recovery-supervisor.sh} \
                "$out/bin/mt7921-fresh-laa-diagnostic-supervisor" \
                --subst-var-by runtime_path /run/current-system/sw/bin \
                --subst-var-by wifi_driver_lab /run/current-system/sw/bin/wifi-driver-lab \
                --subst-var-by wifi_lab_watchdog /run/current-system/sw/bin/wifi-lab-watchdog \
                --subst-var-by validation_launcher ${mt7921-fresh-laa-diagnostic}/bin/mt7921-full-firmware-validation \
                --subst-var-by artifact_identity ${mt7921-fresh-laa-diagnostic}/share/mt7921-full-firmware-validation/artifact-identity.json \
                --subst-var-by recovery_samples 45 --subst-var-by sys_root /sys \
                --subst-var-by run_root /run --subst-var-by var_root /var \
                --subst-var-by id_command ${pkgs.coreutils}/bin/id \
                --subst-var-by native_client_mac 8a:fd:2a:8b:70:5a \
                --subst-var-by session_client_mac 02:7d:91:4c:b8:3e \
                --subst-var-by identity_mode fixed-fresh-laa-diagnostic
              chmod 0755 "$out/bin/mt7921-fresh-laa-diagnostic-supervisor"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-fresh-laa-diagnostic-supervisor"
              ! grep -Eq '@[a-z_]+@' "$out/bin/mt7921-fresh-laa-diagnostic-supervisor"
              grep -F 'native_identity_restored' "$out/bin/mt7921-fresh-laa-diagnostic-supervisor"
              grep -F 'session_client_mac=02:7d:91:4c:b8:3e' "$out/bin/mt7921-fresh-laa-diagnostic-supervisor"
            '';

          mt7921-full-firmware-validation-launcher-test = pkgs.runCommand
            "mt7921-full-firmware-validation-launcher-test"
            { nativeBuildInputs = [ pkgs.coreutils pkgs.gnused pkgs.gnugrep ]; }
            ''
              mkdir -p work/bin work/var
              cat > work/bin/driver <<'EOF'
              #!${pkgs.runtimeShell}
              set -eu
              if [ "''${1-}" = --artifact-identity ]; then ${pkgs.coreutils}/bin/cat "$PWD/work/var/identity"; exit; fi
              ${pkgs.coreutils}/bin/env | ${pkgs.coreutils}/bin/sort > "$PWD/transcript"
              printf 'ARGV <%s>\n' "$*" >> "$PWD/transcript"
              EOF
              cat > work/bin/snapshot <<'EOF'
              #!${pkgs.runtimeShell}
              printf snapshot-ok
              EOF
              chmod +x work/bin/driver work/bin/snapshot
              printf 'Passphrase=eight-by\n' > work/var/ph1.psk
              : > work/var/regulatory.db
              printf '%s\n' '{"artifact_identity":"mt7921-driver-v11","flavor":"full-firmware-production","enabled_operation":"run-one-shot-sae-auth","bss_wire_contract":"connac2-bss-wire-v1","active_capable":true}' > work/var/identity
              substitute ${./nix/mt7921-full-firmware-validation-launcher.sh} work/launcher \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by driver "$PWD/work/bin/driver" \
                --subst-var-by snapshot_generator "$PWD/work/bin/snapshot" \
                --subst-var-by regulatory_db "$PWD/work/var/regulatory.db" \
                --subst-var-by regulatory_source_sha256 0000000000000000000000000000000000000000000000000000000000000000 \
                --subst-var-by credential_file "$PWD/work/var/ph1.psk" \
                --subst-var-by artifact_identity "$PWD/work/var/identity" \
                --subst-var-by session_client_mac 8a:fd:2a:8b:70:5a \
                --subst-var-by cat ${pkgs.coreutils}/bin/cat \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by mktemp ${pkgs.coreutils}/bin/mktemp \
                --subst-var-by wc ${pkgs.coreutils}/bin/wc \
                --subst-var-by stat ${pkgs.coreutils}/bin/stat \
                --subst-var-by rm ${pkgs.coreutils}/bin/rm \
                --subst-var-by env ${pkgs.coreutils}/bin/env
              chmod +x work/launcher
              work/launcher --artifact-identity | grep -F '"artifact_identity":"mt7921-driver-v11"'
              env -i DRV_PCI_BDF=0000:05:00.0 DRV_IOMMU_GROUP=17 \
                DRV_VFIO_DEVICE=/dev/vfio/devices/vfio17 \
                DRV_LAB_SAFETY_STATE=/run/wifi-driver-lab/fixed.state.safety work/launcher
              grep -Fx 'DRV_ACTIVE_CLIENT=1' transcript
              grep -Fx 'DRV_SAE_CLIENT_MAC=8a:fd:2a:8b:70:5a' transcript
              grep -Fx 'ARGV <--run-one-shot-sae-auth>' transcript
              touch "$out"
            '';

          mt7921-full-firmware-validation-supervisor = pkgs.runCommand
            "mt7921-full-firmware-validation-supervisor"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]; }
            ''
              mkdir -p "$out/bin"
              substitute ${./crates/mt7921-port-spike/lab/selector-write-recovery-supervisor.sh} \
                "$out/bin/mt7921-full-firmware-validation-supervisor" \
                --subst-var-by runtime_path /run/current-system/sw/bin \
                --subst-var-by wifi_driver_lab /run/current-system/sw/bin/wifi-driver-lab \
                --subst-var-by wifi_lab_watchdog /run/current-system/sw/bin/wifi-lab-watchdog \
                --subst-var-by validation_launcher ${mt7921-full-firmware-validation}/bin/mt7921-full-firmware-validation \
                --subst-var-by artifact_identity ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json \
                --subst-var-by flavor full-firmware-production \
                --subst-var-by recovery_samples 45 \
                --subst-var-by sys_root /sys \
                --subst-var-by run_root /run \
                --subst-var-by var_root /var \
                --subst-var-by id_command ${pkgs.coreutils}/bin/id \
                --subst-var-by native_client_mac 8a:fd:2a:8b:70:5a \
                --subst-var-by session_client_mac 8a:fd:2a:8b:70:5a \
                --subst-var-by identity_mode native-handoff
              chmod 0755 "$out/bin/mt7921-full-firmware-validation-supervisor"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-validation-supervisor"
              grep -F 'connected Wi-Fi target drifted from fixed ajay validation policy' \
                "$out/bin/mt7921-full-firmware-validation-supervisor"
              grep -F 'bdf=%s timeout_seconds=300 watchdog_owner=' "$out/bin/mt7921-full-firmware-validation-supervisor"
              grep -Fx '"$wifi_driver_lab" "$bdf" 300 -- "$@" &' "$out/bin/mt7921-full-firmware-validation-supervisor"
              identity=${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json
              grep -F '"association_request_contract":"client-mlme+device-query+pinned-regdb"' "$identity"
              grep -F '"bss_wire_contract":"connac2-bss-wire-v1"' "$identity"
            '';

          mt7921-full-firmware-validation-recovery-status = pkgs.stdenv.mkDerivation {
            pname = "mt7921-full-firmware-validation-recovery-status";
            version = "1";
            dontUnpack = true;
            nativeBuildInputs = [ pkgs.bash pkgs.gnugrep ];
            doInstallCheck = true;
            meta.mainProgram = "mt7921-full-firmware-validation-recovery-status";
            installPhase = ''
              mkdir -p "$out/bin"
              substitute ${./nix/mt7921-full-firmware-validation-recovery-status.sh} \
                "$out/bin/mt7921-full-firmware-validation-recovery-status" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by run_root /run \
                --subst-var-by sys_root /sys \
                --subst-var-by id ${pkgs.coreutils}/bin/id \
                --subst-var-by basename ${pkgs.coreutils}/bin/basename \
                --subst-var-by readlink ${pkgs.coreutils}/bin/readlink \
                --subst-var-by systemctl ${pkgs.systemd}/bin/systemctl \
                --subst-var-by ip ${pkgs.iproute2}/bin/ip \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by awk ${pkgs.gawk}/bin/awk \
                --subst-var-by ping ${pkgs.iputils}/bin/ping
              chmod 0755 "$out/bin/mt7921-full-firmware-validation-recovery-status"
            '';
            installCheckPhase = ''
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-validation-recovery-status"
              ! grep -Eq '@[a-z_]+' "$out/bin/mt7921-full-firmware-validation-recovery-status"
              grep -F 'contract=mt7921-full-firmware-recovery-status-v1' "$out/bin/mt7921-full-firmware-validation-recovery-status"
            '';
          };

          mt7921-full-firmware-validation-recovery-status-test = pkgs.runCommand
            "mt7921-full-firmware-validation-recovery-status-test"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]; }
            ''
              mkdir -p root/run/wifi-driver-lab root/sys/bus/pci/devices/0000:05:00.0/net root/sys/drivers/mt7921e bin
              ln -s ../../../../drivers/mt7921e root/sys/bus/pci/devices/0000:05:00.0/driver
              touch root/sys/bus/pci/devices/0000:05:00.0/net/wlan-test
              cat > bin/id <<'EOF'
              #!${pkgs.runtimeShell}
              echo 0
              EOF
              cat > bin/systemctl <<'EOF'
              #!${pkgs.runtimeShell}
              test "$1" = is-active && test "$2" = --quiet && test "$3" = iwd.service
              EOF
              cat > bin/ip <<'EOF'
              #!${pkgs.runtimeShell}
              case "$1:$2" in
                -4:addr) echo 'inet 192.0.2.2/24' ;;
                route:show) echo 'default via 192.0.2.1 dev wlan-test' ;;
                *) exit 1 ;;
              esac
              EOF
              cat > bin/ping <<'EOF'
              #!${pkgs.runtimeShell}
              exit 0
              EOF
              chmod +x bin/*
              substitute ${./nix/mt7921-full-firmware-validation-recovery-status.sh} status \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by run_root "$PWD/root/run" \
                --subst-var-by sys_root "$PWD/root/sys" \
                --subst-var-by id "$PWD/bin/id" \
                --subst-var-by basename ${pkgs.coreutils}/bin/basename \
                --subst-var-by readlink ${pkgs.coreutils}/bin/readlink \
                --subst-var-by systemctl "$PWD/bin/systemctl" \
                --subst-var-by ip "$PWD/bin/ip" \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by awk ${pkgs.gawk}/bin/awk \
                --subst-var-by ping "$PWD/bin/ping"
              chmod +x status
              test "$(./status --version)" = mt7921-full-firmware-recovery-status-v1
              ./status --idle
              set +e
              ./status --quarantined
              rc=$?
              set -e
              test "$rc" -eq 1
              ./status --native-ready 0000:05:00.0
              : > root/run/wifi-driver-lab/a.state
              ! ./status --idle
              echo UNSAFE > root/run/wifi-driver-lab/a.state.safety
              ./status --quarantined
              echo SAFE > root/run/wifi-driver-lab/a.state.safety
              ! ./status --quarantined
              ! ./status --native-ready 0000:00:00.0
              set +e
              ./status arbitrary
              rc=$?
              set -e
              test "$rc" -eq 64
              touch "$out"
            '';

          mt7921-full-firmware-validation-remote-entry =
            let
              recoveryStatusRegisteredHash = pkgs.runCommand
                "mt7921-full-firmware-validation-recovery-status-registered-hash"
                {
                  __structuredAttrs = true;
                  exportReferencesGraph.recoveryStatus = [ mt7921-full-firmware-validation-recovery-status ];
                  nativeBuildInputs = [ pkgs.jq ];
                }
                ''
                  out="''${outputs[out]}"
                  ${pkgs.jq}/bin/jq -er --arg path '${mt7921-full-firmware-validation-recovery-status}' \
                    '.recoveryStatus[] | select(.path == $path) | .narHash' \
                    "$NIX_ATTRS_JSON_FILE" > "$out"
                '';
            in
            pkgs.stdenv.mkDerivation {
            pname = "mt7921-full-firmware-validation-remote-entry";
            version = "0.1.0";
            dontUnpack = true;
            nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep ];
            doInstallCheck = true;
            meta.mainProgram = "mt7921-full-firmware-validation-remote-entry";
            installPhase = ''
              runHook preInstall
              mkdir -p "$out/bin"
              substitute ${./nix/mt7921-full-firmware-validation-remote-entry.sh} \
                "$out/bin/mt7921-full-firmware-validation-remote-entry" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by ssh ${pkgs.openssh}/bin/ssh \
                --subst-var-by tailscale ${pkgs.tailscale}/bin/tailscale \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by sleep ${pkgs.coreutils}/bin/sleep \
                --subst-var-by timeout ${pkgs.coreutils}/bin/timeout \
                --subst-var-by recovery_call_timeout_seconds 15 \
                --subst-var-by home /home/maan2003 \
                --subst-var-by xdg_runtime_dir /run/user/1002 \
                --subst-var-by target_root /nix/store/0vfijil292q1scv8wmg9gylj5c68xynj-mt7921-full-firmware-validation-root-entry \
                --subst-var-by target_package /nix/store/sr1hy24m754pms8idnjsq5ir45y4njdv-mt7921-full-firmware-validation-0.1.0 \
                --subst-var-by target_manifest /nix/store/vx717636m37lh0icvbh6hzwiyp5p7pgx-mt7921-full-firmware-validation-manifest \
                --subst-var-by target_supervisor /nix/store/fxr4kd4xi26adwywd2k6fvl96k84wbd0-mt7921-full-firmware-validation-supervisor \
                --subst-var-by target_nix_store /nix/store/m9gfpnfrwdhr2cqakrfki9p73rjlfqgd-lix-2.95.2/bin/nix-store \
                --subst-var-by target_sha256sum /nix/store/mp8s10fwm685azvvv1qq7zyf7iajjlj8-coreutils-9.11/bin/sha256sum \
                --subst-var-by target_recovery_package ${mt7921-full-firmware-validation-recovery-status} \
                --subst-var-by target_recovery_helper ${mt7921-full-firmware-validation-recovery-status}/bin/mt7921-full-firmware-validation-recovery-status \
                --subst-var-by target_recovery_registered_hash "$(cat ${recoveryStatusRegisteredHash})" \
                --subst-var-by target_recovery_sha256 "$(sha256sum ${mt7921-full-firmware-validation-recovery-status}/bin/mt7921-full-firmware-validation-recovery-status | cut -d ' ' -f1)" \
                --subst-var-by target_sudo /run/wrappers/bin/sudo \
                --subst-var-by target_root_registered_hash sha256:0shrf2qwqw1mf7h3jx3vi0kabrvv63vpaw8vh9i6y63wkrzrs8is \
                --subst-var-by target_package_registered_hash sha256:1hn998lnpbagjl3mpsr4c9wh47lrz6zg8nfzmv7mdygdq29vq1ww \
                --subst-var-by target_manifest_registered_hash sha256:17nczmm58936snmr33f4d6w1p0v36l8p22qsblgnllil0aip0bg8 \
                --subst-var-by target_supervisor_registered_hash sha256:0hy4bbd69xlw73f1w41ppsj8dd24arwylzdaz91z45ifa68fgpci \
                --subst-var-by target_entry_sha256 a46a5b1d5933e7f9f1fe594777411b7bc246026434897f7a13a1ade9da362f5e \
                --subst-var-by target_manifest_sha256 51a4410f8ecdf80f46b5395ce7ac1c4d507cad044c00d2d17ee9e2806b17da8c \
                --subst-var-by target_supervisor_sha256 22a80c25d05239529c8df40c7480a6983c7cc12033538b6eac391de3f77a61db \
                --subst-var-by target_identity_sha256 b41591b6a867bea9ec989debe00c05b99d8a4a142579958acdf24e784a718aa2 \
                --subst-var-by target_launcher_sha256 3b821ef1a8cb655e0cb42459f6911d497d7177449f35e1ca993704dfb2f84987 \
                --subst-var-by transport_contract openssh-absolute+ssh-config-disabled+fixed-home-key-known-hosts+identities-only+connect-timeout-10+server-alive-2x3+strict-known-hosts+tailscale-absolute-userspace-socket+fixed-user-host+verify-path+versioned-bounded-read-only-recovery-v2
              chmod 0755 "$out/bin/mt7921-full-firmware-validation-remote-entry"
              runHook postInstall
            '';
            installCheckPhase = ''
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-validation-remote-entry"
              ! grep -Eq '@[a-z_]+' "$out/bin/mt7921-full-firmware-validation-remote-entry"
              grep -F '${pkgs.openssh}/bin/ssh' "$out/bin/mt7921-full-firmware-validation-remote-entry"
              grep -F '${pkgs.tailscale}/bin/tailscale' "$out/bin/mt7921-full-firmware-validation-remote-entry"
              grep -F 'StrictHostKeyChecking=yes' "$out/bin/mt7921-full-firmware-validation-remote-entry"
              grep -F 'remote "$target_entry"' "$out/bin/mt7921-full-firmware-validation-remote-entry"
              ! grep -F 'watchdog disarm' "$out/bin/mt7921-full-firmware-validation-remote-entry"
            '';
            };

          mt7921-full-firmware-validation-delivery-manifest =
            let
              remoteEntryRegisteredHash = pkgs.runCommand
                "mt7921-full-firmware-validation-remote-entry-registered-hash"
                {
                  __structuredAttrs = true;
                  exportReferencesGraph.remoteEntry = [ mt7921-full-firmware-validation-remote-entry ];
                  nativeBuildInputs = [ pkgs.jq ];
                }
                ''
                  out="''${outputs[out]}"
                  ${pkgs.jq}/bin/jq -er --arg path '${mt7921-full-firmware-validation-remote-entry}' \
                    '.remoteEntry[] | select(.path == $path) | .narHash' \
                  "$NIX_ATTRS_JSON_FILE" > "$out"
                '';
              recoveryStatusRegisteredHash = pkgs.runCommand
                "mt7921-full-firmware-validation-delivery-recovery-status-registered-hash"
                {
                  __structuredAttrs = true;
                  exportReferencesGraph.recoveryStatus = [ mt7921-full-firmware-validation-recovery-status ];
                  nativeBuildInputs = [ pkgs.jq ];
                }
                ''
                  out="''${outputs[out]}"
                  ${pkgs.jq}/bin/jq -er --arg path '${mt7921-full-firmware-validation-recovery-status}' \
                    '.recoveryStatus[] | select(.path == $path) | .narHash' \
                    "$NIX_ATTRS_JSON_FILE" > "$out"
                '';
            in
            pkgs.runCommand "mt7921-full-firmware-validation-delivery-manifest"
              { nativeBuildInputs = [ pkgs.coreutils pkgs.gnugrep ]; }
              ''
                entry=${mt7921-full-firmware-validation-remote-entry}/bin/mt7921-full-firmware-validation-remote-entry
                cat > "$out" <<EOF
                REMOTE_ENTRY=$entry
                REMOTE_ENTRY_SHA256=$(sha256sum "$entry" | cut -d ' ' -f1)
                REMOTE_ENTRY_REGISTERED_HASH=$(cat ${remoteEntryRegisteredHash})
                REMOTE_TRANSPORT_CONTRACT=openssh-absolute+ssh-config-disabled+fixed-home-key-known-hosts+identities-only+connect-timeout-10+server-alive-2x3+strict-known-hosts+tailscale-absolute-userspace-socket+fixed-user-host+verify-path+versioned-bounded-read-only-recovery-v2
                REMOTE_TARGET=user@no-plastic
                REMOTE_TARGET_ROOT=/nix/store/0vfijil292q1scv8wmg9gylj5c68xynj-mt7921-full-firmware-validation-root-entry
                REMOTE_TARGET_ROOT_REGISTERED_HASH=sha256:0shrf2qwqw1mf7h3jx3vi0kabrvv63vpaw8vh9i6y63wkrzrs8is
                REMOTE_TARGET_ENTRY_SHA256=a46a5b1d5933e7f9f1fe594777411b7bc246026434897f7a13a1ade9da362f5e
                REMOTE_TARGET_PACKAGE=/nix/store/sr1hy24m754pms8idnjsq5ir45y4njdv-mt7921-full-firmware-validation-0.1.0
                REMOTE_TARGET_PACKAGE_REGISTERED_HASH=sha256:1hn998lnpbagjl3mpsr4c9wh47lrz6zg8nfzmv7mdygdq29vq1ww
                REMOTE_TARGET_MANIFEST=/nix/store/vx717636m37lh0icvbh6hzwiyp5p7pgx-mt7921-full-firmware-validation-manifest
                REMOTE_TARGET_MANIFEST_REGISTERED_HASH=sha256:17nczmm58936snmr33f4d6w1p0v36l8p22qsblgnllil0aip0bg8
                REMOTE_TARGET_MANIFEST_SHA256=51a4410f8ecdf80f46b5395ce7ac1c4d507cad044c00d2d17ee9e2806b17da8c
                REMOTE_TARGET_SUPERVISOR=/nix/store/fxr4kd4xi26adwywd2k6fvl96k84wbd0-mt7921-full-firmware-validation-supervisor
                REMOTE_TARGET_SUPERVISOR_REGISTERED_HASH=sha256:0hy4bbd69xlw73f1w41ppsj8dd24arwylzdaz91z45ifa68fgpci
                REMOTE_TARGET_SUPERVISOR_SHA256=22a80c25d05239529c8df40c7480a6983c7cc12033538b6eac391de3f77a61db
                REMOTE_TARGET_IDENTITY_SHA256=b41591b6a867bea9ec989debe00c05b99d8a4a142579958acdf24e784a718aa2
                REMOTE_TARGET_LAUNCHER_SHA256=3b821ef1a8cb655e0cb42459f6911d497d7177449f35e1ca993704dfb2f84987
                REMOTE_ACTIVE_ARGC=0
                REMOTE_PLAN_ARGV=--plan
                REMOTE_RECOVERY_CONTRACT=sudo-n-exact-helper-poll-only-bounded-no-disarm
                REMOTE_RECOVERY_STATUS_PACKAGE=${mt7921-full-firmware-validation-recovery-status}
                REMOTE_RECOVERY_STATUS_HELPER=${mt7921-full-firmware-validation-recovery-status}/bin/mt7921-full-firmware-validation-recovery-status
                REMOTE_RECOVERY_STATUS_HELPER_SHA256=$(sha256sum ${mt7921-full-firmware-validation-recovery-status}/bin/mt7921-full-firmware-validation-recovery-status | cut -d ' ' -f1)
                REMOTE_RECOVERY_STATUS_REGISTERED_HASH=$(cat ${recoveryStatusRegisteredHash})
                REMOTE_RECOVERY_STATUS_CONTRACT=mt7921-full-firmware-recovery-status-v1
                EOF
                grep -Fx "REMOTE_ENTRY=$entry" "$out"
                grep -Eq '^REMOTE_ENTRY_SHA256=[0-9a-f]{64}$' "$out"
                grep -Eq '^REMOTE_ENTRY_REGISTERED_HASH=sha256:[0123456789abcdfghijklmnpqrsvwxyz]{52}$' "$out"
              '';

          mt7921-full-firmware-validation-remote-entry-test = pkgs.runCommand
            "mt7921-full-firmware-validation-remote-entry-test"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep pkgs.python3 ]; }
            ''
              mkdir -p work/home/.ssh work/runtime/tailscale
              : > work/home/.ssh/no-plastic
              : > work/home/.ssh/known_hosts
              chmod 0600 work/home/.ssh/no-plastic
              cat > work/tailscale-stub <<'EOF'
              #!${pkgs.runtimeShell}
              printf '<%s>' "$@" >> "$PWD/tailscale.transcript"
              printf '\n' >> "$PWD/tailscale.transcript"
              exit 0
              EOF
              cat > work/sleep-stub <<'EOF'
              #!${pkgs.runtimeShell}
              exit 0
              EOF
              cat > work/ssh-stub <<'EOF'
              #!${pkgs.runtimeShell}
              set -euo pipefail
              printf '<%s>' "$@" >> "$PWD/ssh.transcript"
              printf '\n' >> "$PWD/ssh.transcript"
              expected_proxy="ProxyCommand=$PWD/work/tailscale-stub --socket=$XDG_RUNTIME_DIR/tailscale/tailscaled.sock nc %h %p"
              test "$1" = -F && test "$2" = /dev/null
              test "$3" = -i
              test "$4" = "$HOME/.ssh/no-plastic"
              test "$5" = -o && test "$6" = BatchMode=yes
              test "$7" = -o && test "$8" = IdentitiesOnly=yes
              test "$9" = -o && test "''${10}" = ConnectTimeout=10
              test "''${11}" = -o && test "''${12}" = ServerAliveInterval=2
              test "''${13}" = -o && test "''${14}" = ServerAliveCountMax=3
              test "''${15}" = -o && test "''${16}" = StrictHostKeyChecking=yes
              test "''${17}" = -o && test "''${18}" = "UserKnownHostsFile=$HOME/.ssh/known_hosts"
              test "''${19}" = -o && test "''${20}" = GlobalKnownHostsFile=/dev/null
              test "''${21}" = -o && test "''${22}" = "$expected_proxy"
              test "''${23}" = user@no-plastic
              "$PWD/work/tailscale-stub" "--socket=$XDG_RUNTIME_DIR/tailscale/tailscaled.sock" nc no-plastic 22
              shift 23
              if [ "''${MODE-}" = hostkey ]; then
                echo 'Host key verification failed.' >&2
                exit 255
              fi
              root=/nix/store/0vfijil292q1scv8wmg9gylj5c68xynj-mt7921-full-firmware-validation-root-entry
              package=/nix/store/sr1hy24m754pms8idnjsq5ir45y4njdv-mt7921-full-firmware-validation-0.1.0
              manifest=/nix/store/vx717636m37lh0icvbh6hzwiyp5p7pgx-mt7921-full-firmware-validation-manifest
              supervisor=/nix/store/fxr4kd4xi26adwywd2k6fvl96k84wbd0-mt7921-full-firmware-validation-supervisor
              entry=$root/bin/mt7921-full-firmware-validation-root
              identity=$package/share/mt7921-full-firmware-validation/artifact-identity.json
              launcher=$package/bin/mt7921-full-firmware-validation
              supervisor_file=$supervisor/bin/mt7921-full-firmware-validation-supervisor
              if [ "$1" = /target/nix-store ] && [ "$2" = --verify-path ]; then
                test "$#" -eq 3
                exit 0
              elif [ "$1" = /target/nix-store ]; then
                test "$2" = -q && test "$3" = --hash
                if [ "''${MODE-}" = wronghash ] && [ "$4" = "$root" ]; then
                  echo sha256:wrong
                  exit 0
                fi
                case "$4" in
                  "$root") echo sha256:0shrf2qwqw1mf7h3jx3vi0kabrvv63vpaw8vh9i6y63wkrzrs8is ;;
                  "$package") echo sha256:1hn998lnpbagjl3mpsr4c9wh47lrz6zg8nfzmv7mdygdq29vq1ww ;;
                  "$manifest") echo sha256:17nczmm58936snmr33f4d6w1p0v36l8p22qsblgnllil0aip0bg8 ;;
                  "$supervisor") echo sha256:0hy4bbd69xlw73f1w41ppsj8dd24arwylzdaz91z45ifa68fgpci ;;
                  /target/recovery-package) echo sha256:recoveryregisteredhash00000000000000000000000000000000 ;;
                  *) exit 90 ;;
                esac
              elif [ "$1" = /target/sha256sum ]; then
                case "$2" in
                  "$entry") hash=a46a5b1d5933e7f9f1fe594777411b7bc246026434897f7a13a1ade9da362f5e ;;
                  "$manifest") hash=51a4410f8ecdf80f46b5395ce7ac1c4d507cad044c00d2d17ee9e2806b17da8c ;;
                  "$supervisor_file") hash=22a80c25d05239529c8df40c7480a6983c7cc12033538b6eac391de3f77a61db ;;
                  "$identity") hash=b41591b6a867bea9ec989debe00c05b99d8a4a142579958acdf24e784a718aa2 ;;
                  "$launcher") hash=3b821ef1a8cb655e0cb42459f6911d497d7177449f35e1ca993704dfb2f84987 ;;
                  /target/recovery) hash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
                  *) exit 91 ;;
                esac
                printf '%s  %s\n' "$hash" "$2"
              elif [ "$1" = "$entry" ] && [ "''${2-}" = --plan ] && [ "$#" -eq 2 ]; then
                printf 'ROOT_ENTRY privilege=sudo_-n manifest=%s manifest_sha256=51a4410f8ecdf80f46b5395ce7ac1c4d507cad044c00d2d17ee9e2806b17da8c supervisor=%s launcher=%s flavor=full-firmware-production active_capable=true bdf=0000:05:00.0 mode=--plan\n' "$manifest" "$supervisor_file" "$launcher"
                printf 'PLAN mode=inert hardware_handoff=false supervisor=%s supervisor_sha256=22a80c25d05239529c8df40c7480a6983c7cc12033538b6eac391de3f77a61db launcher=%s launcher_sha256=3b821ef1a8cb655e0cb42459f6911d497d7177449f35e1ca993704dfb2f84987\n' "$supervisor_file" "$launcher"
              elif [ "$1" = "$entry" ] && [ "$#" -eq 1 ]; then
                echo active >> "$PWD/active.calls"
                if [ "''${MODE-}" = unknown ] || [ "''${MODE-}" = hang ]; then exit 255; fi
                exit 0
              elif [ "$1" = /target/sudo ] && [ "$2" = -n ] && [ "$3" = /target/recovery ]; then
                if [ "''${MODE-}" = oldhelper ]; then
                  echo 'usage: wifi-driver-lab PCI_BDF TIMEOUT_SECONDS -- COMMAND [ARG ...]' >&2
                  exit 2
                fi
                if [ "''${MODE-}" = hang ] && [ -e "$PWD/active.calls" ] && [ ! -e "$PWD/hang.once" ]; then
                  touch "$PWD/hang.once"
                  ${pkgs.coreutils}/bin/sleep 5
                fi
                case "$4" in
                  --version) echo mt7921-full-firmware-recovery-status-v1 ;;
                  --quarantined) exit 1 ;;
                  --idle) exit 0 ;;
                  --native-ready) test "$5" = 0000:05:00.0; exit 0 ;;
                  *) exit 92 ;;
                esac
              else
                exit 93
              fi
              EOF
              chmod 0755 work/{ssh,tailscale,sleep}-stub
              substitute ${./nix/mt7921-full-firmware-validation-remote-entry.sh} work/entry \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by ssh "$PWD/work/ssh-stub" \
                --subst-var-by tailscale "$PWD/work/tailscale-stub" \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by sleep "$PWD/work/sleep-stub" \
                --subst-var-by timeout ${pkgs.coreutils}/bin/timeout \
                --subst-var-by recovery_call_timeout_seconds 1 \
                --subst-var-by home "$PWD/work/home" \
                --subst-var-by xdg_runtime_dir "$PWD/work/runtime" \
                --subst-var-by target_root /nix/store/0vfijil292q1scv8wmg9gylj5c68xynj-mt7921-full-firmware-validation-root-entry \
                --subst-var-by target_package /nix/store/sr1hy24m754pms8idnjsq5ir45y4njdv-mt7921-full-firmware-validation-0.1.0 \
                --subst-var-by target_manifest /nix/store/vx717636m37lh0icvbh6hzwiyp5p7pgx-mt7921-full-firmware-validation-manifest \
                --subst-var-by target_supervisor /nix/store/fxr4kd4xi26adwywd2k6fvl96k84wbd0-mt7921-full-firmware-validation-supervisor \
                --subst-var-by target_nix_store /target/nix-store \
                --subst-var-by target_sha256sum /target/sha256sum \
                --subst-var-by target_recovery_helper /target/recovery \
                --subst-var-by target_recovery_package /target/recovery-package \
                --subst-var-by target_recovery_registered_hash sha256:recoveryregisteredhash00000000000000000000000000000000 \
                --subst-var-by target_recovery_sha256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
                --subst-var-by target_sudo /target/sudo \
                --subst-var-by target_root_registered_hash sha256:0shrf2qwqw1mf7h3jx3vi0kabrvv63vpaw8vh9i6y63wkrzrs8is \
                --subst-var-by target_package_registered_hash sha256:1hn998lnpbagjl3mpsr4c9wh47lrz6zg8nfzmv7mdygdq29vq1ww \
                --subst-var-by target_manifest_registered_hash sha256:17nczmm58936snmr33f4d6w1p0v36l8p22qsblgnllil0aip0bg8 \
                --subst-var-by target_supervisor_registered_hash sha256:0hy4bbd69xlw73f1w41ppsj8dd24arwylzdaz91z45ifa68fgpci \
                --subst-var-by target_entry_sha256 a46a5b1d5933e7f9f1fe594777411b7bc246026434897f7a13a1ade9da362f5e \
                --subst-var-by target_manifest_sha256 51a4410f8ecdf80f46b5395ce7ac1c4d507cad044c00d2d17ee9e2806b17da8c \
                --subst-var-by target_supervisor_sha256 22a80c25d05239529c8df40c7480a6983c7cc12033538b6eac391de3f77a61db \
                --subst-var-by target_identity_sha256 b41591b6a867bea9ec989debe00c05b99d8a4a142579958acdf24e784a718aa2 \
                --subst-var-by target_launcher_sha256 3b821ef1a8cb655e0cb42459f6911d497d7177449f35e1ca993704dfb2f84987 \
                --subst-var-by transport_contract test-transport-v1
              chmod 0755 work/entry
              ${pkgs.python3}/bin/python - <<'PY' &
              import socket
              s = socket.socket(socket.AF_UNIX)
              s.bind('work/runtime/tailscale/tailscaled.sock')
              s.listen()
              s.accept()
              PY
              socket_pid=$!
              trap 'kill "$socket_pid" 2>/dev/null || true' EXIT
              while [ ! -S work/runtime/tailscale/tailscaled.sock ]; do sleep 0.01; done
              export HOME=$PWD/work/home XDG_RUNTIME_DIR=$PWD/work/runtime

              work/entry --plan > plan
              grep -F 'REMOTE_PLAN hardware_handoff=false watchdog_operation=false' plan
              test ! -e active.calls

              : > ssh.transcript
              work/entry > active
              test "$(wc -l < active.calls)" -eq 1
              grep -F "</nix/store/0vfijil292q1scv8wmg9gylj5c68xynj-mt7921-full-firmware-validation-root-entry/bin/mt7921-full-firmware-validation-root>" ssh.transcript

              rm active.calls
              : > ssh.transcript
              set +e
              MODE=unknown work/entry > unknown 2>unknown.error
              rc=$?
              set -e
              test "$rc" -eq 75
              grep -F 'target recovery is complete; experiment outcome remains unknown' unknown.error
              test "$(wc -l < active.calls)" -eq 1
              grep -F '</target/sudo><-n></target/recovery><--quarantined>' ssh.transcript
              grep -F '</target/sudo><-n></target/recovery><--idle>' ssh.transcript
              grep -F '</target/sudo><-n></target/recovery><--native-ready><0000:05:00.0>' ssh.transcript

              rm -f active.calls hang.once
              set +e
              MODE=hang work/entry > hang.out 2>hang.error
              rc=$?
              set -e
              test "$rc" -eq 75
              test -e hang.once
              grep -F 'target recovery is complete; experiment outcome remains unknown' hang.error

              rm -f active.calls
              set +e
              MODE=oldhelper work/entry --plan >old.out 2>old.error
              rc=$?
              set -e
              test "$rc" -eq 1
              test ! -e active.calls
              grep -F 'fixed recovery status version query failed' old.error

              rm -f active.calls
              set +e
              MODE=hostkey work/entry >hostkey.out 2>hostkey.error
              rc=$?
              set -e
              test "$rc" -eq 1
              test ! -e active.calls
              grep -F 'remote store content verification failed' hostkey.error

              set +e
              MODE=wronghash work/entry >wrong.out 2>wrong.error
              rc=$?
              set -e
              test "$rc" -eq 1
              test ! -e active.calls
              grep -F 'remote registered hash mismatch' wrong.error

              set +e
              work/entry arbitrary >args.out 2>args.error
              rc=$?
              set -e
              test "$rc" -eq 64
              test ! -e active.calls
              grep -F 'accepts no arguments except --plan' args.error
              test -s tailscale.transcript
              touch "$out"
            '';

          mt7921-full-firmware-validation-manifest =
            let
              package = mt7921-full-firmware-validation;
              supervisorPackage = mt7921-full-firmware-validation-supervisor;
              closure = pkgs.closureInfo { rootPaths = [ package supervisorPackage ]; };
            in pkgs.runCommand "mt7921-full-firmware-validation-manifest"
              { nativeBuildInputs = [ pkgs.coreutils pkgs.gnugrep pkgs.gnused ]; } ''
                identity=$package/share/mt7921-full-firmware-validation/artifact-identity.json
                get() { sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p" "$identity"; }
                cat > "$out" <<EOF
                PACKAGE=$package
                LAUNCHER=$package/bin/mt7921-full-firmware-validation
                ELF=$package/bin/mt7921-full-firmware-validation-driver
                ARTIFACT_IDENTITY=$identity
                SOURCE_IDENTITY_SHA256=$(get source_identity_sha256)
                PROJECT_CORE_SOURCE_SHA256=$(get project_core_source_sha256)
                COMPOSITE_ARTIFACT_SOURCE_SHA256=$(get composite_artifact_source_sha256)
                BSS_WIRE_CONTRACT=connac2-bss-wire-v1
                FLAVOR=full-firmware-production
                ACTIVE_CAPABLE=true
                FD_CONTRACT=credential-fd3+snapshot-fd4+immediate-eof
                SUPERVISOR=$supervisorPackage/bin/mt7921-full-firmware-validation-supervisor
                CLOSURE_SHA256=$(sort ${closure}/store-paths | sha256sum | cut -d ' ' -f1)
                PCI_BDF=0000:05:00.0
                TIMEOUT_SECONDS=300
                OPERATION=--run-one-shot-sae-auth
                MODE=DRV_ACTIVE_CLIENT=1
                EOF
                grep -Fx 'BSS_WIRE_CONTRACT=connac2-bss-wire-v1' "$out"
              '';

          mt7921-full-firmware-validation-root-entry = pkgs.runCommand
            "mt7921-full-firmware-validation-root-entry"
            {
              nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep pkgs.gnused ];
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
                --subst-var-by flavor full-firmware-production \
                --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                --subst-var-by cut ${pkgs.coreutils}/bin/cut \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by cat ${pkgs.coreutils}/bin/cat
              chmod 0755 "$out/bin/mt7921-full-firmware-validation-root"
              ! grep -Eq '@[a-z_]+@' "$out/bin/mt7921-full-firmware-validation-root"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-validation-root"
            '';

          mt7921-fresh-laa-diagnostic-manifest = pkgs.runCommand
            "mt7921-fresh-laa-diagnostic-manifest" { } ''
              sed \
                -e 's|${mt7921-full-firmware-validation}|${mt7921-fresh-laa-diagnostic}|g' \
                -e 's|${mt7921-full-firmware-validation-supervisor}|${mt7921-fresh-laa-diagnostic-supervisor}|g' \
                -e 's/^FLAVOR=.*/FLAVOR=fresh-laa-diagnostic/' \
                ${mt7921-full-firmware-validation-manifest} > "$out"
              cat >> "$out" <<'EOF'
              IDENTITY_MODE=fixed-fresh-laa-diagnostic
              NATIVE_CLIENT_MAC=8a:fd:2a:8b:70:5a
              SESSION_IDENTITY_CONTRACT=single-typed-source-fixed-fresh-laa-dev-muar-bss-omac-sme-mgmt-rx-v1
              RECOVERY_IDENTITY_CONTRACT=native-address-required-before-watchdog-disarm
              EOF
            '';

          mt7921-fresh-laa-diagnostic-root-entry = pkgs.runCommand
            "mt7921-fresh-laa-diagnostic-root-entry"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep pkgs.gnused ]; meta.mainProgram = "mt7921-fresh-laa-diagnostic-root"; }
            ''
              mkdir -p "$out/bin"
              substitute ${./nix/mt7921-full-firmware-validation-root.sh} \
                "$out/bin/mt7921-fresh-laa-diagnostic-root" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by sudo /run/wrappers/bin/sudo \
                --subst-var-by supervisor ${mt7921-fresh-laa-diagnostic-supervisor}/bin/mt7921-fresh-laa-diagnostic-supervisor \
                --subst-var-by launcher ${mt7921-fresh-laa-diagnostic}/bin/mt7921-full-firmware-validation \
                --subst-var-by manifest ${mt7921-fresh-laa-diagnostic-manifest} \
                --subst-var-by artifact_identity ${mt7921-fresh-laa-diagnostic}/share/mt7921-full-firmware-validation/artifact-identity.json \
                --subst-var-by flavor fresh-laa-diagnostic \
                --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                --subst-var-by cut ${pkgs.coreutils}/bin/cut \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by sed ${pkgs.gnused}/bin/sed \
                --subst-var-by cat ${pkgs.coreutils}/bin/cat
              chmod 0755 "$out/bin/mt7921-fresh-laa-diagnostic-root"
              ! grep -Eq '@[a-z_]+@' "$out/bin/mt7921-fresh-laa-diagnostic-root"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-fresh-laa-diagnostic-root"
            '';

          mt7921-fresh-laa-diagnostic-inert-proof = pkgs.runCommand
            "mt7921-fresh-laa-diagnostic-inert-proof"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]; }
            ''
              identity=${mt7921-fresh-laa-diagnostic}/share/mt7921-full-firmware-validation/artifact-identity.json
              launcher=${mt7921-fresh-laa-diagnostic}/bin/mt7921-full-firmware-validation
              supervisor=${mt7921-fresh-laa-diagnostic-supervisor}/bin/mt7921-fresh-laa-diagnostic-supervisor
              root=${mt7921-fresh-laa-diagnostic-root-entry}/bin/mt7921-fresh-laa-diagnostic-root
              test "$("$launcher" --artifact-identity)" = "$(cat "$identity")"
              grep -F 'DRV_SAE_CLIENT_MAC=02:7d:91:4c:b8:3e' "$launcher"
              grep -F 'identity_mode=fixed-fresh-laa-diagnostic' "$supervisor"
              grep -F 'native_identity_restored' "$supervisor"
              grep -F 'hardware_handoff=false' "$supervisor"
              grep -F '/run/wrappers/bin/sudo -n' "$root"
              grep -Fx 'RECOVERY_IDENTITY_CONTRACT=native-address-required-before-watchdog-disarm' ${mt7921-fresh-laa-diagnostic-manifest}
              touch "$out"
            '';

          mt7921-validation-flavor-cross-wire-test = pkgs.runCommand
            "mt7921-validation-flavor-cross-wire-test" { nativeBuildInputs = [ pkgs.gnugrep ]; } ''
              production=${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json
              evidence=${mt7921-rate-power-evidence}/share/mt7921-rate-power-evidence/artifact-identity.json
              grep -F '"artifact_identity":"mt7921-driver-v11"' "$production"
              grep -F '"bss_wire_contract":"connac2-bss-wire-v1"' "$production"
              grep -F '"flavor":"rate-power-evidence-only"' "$evidence"
              ! cmp "$production" "$evidence"
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
                    | if (($rows | length) == 76
                        and ($rows | map(.path) | unique | length) == 76
                        and all($rows[]; (.path | valid_path) and (.narHash | valid_hash)))
                      then $rows else error("invalid \($rows | length)-path registered closure metadata") end
                    | .[] | [.path, .narHash] | @tsv
                  ' "$NIX_ATTRS_JSON_FILE" >"$out/closure.tsv"
                  cut -f1 "$out/closure.tsv" >"$out/closure.paths"
                  test "$(wc -l < "$out/closure.tsv")" -eq 76
                  test "$(awk -F '\t' 'NF == 2 && $1 != "" && $2 != "" { count++ } END { print count+0 }' "$out/closure.tsv")" -eq 76
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
                test "$(wc -l < "$out/share/mt7921-full-firmware-inert-proof/closure.tsv")" -eq 76
                closure_manifest_sha256="$(${pkgs.coreutils}/bin/sha256sum "$out/share/mt7921-full-firmware-inert-proof/closure.tsv" | cut -d' ' -f1)"
                source_identity=$(sed -n 's/.*"source_identity_sha256":"\([0-9a-f]*\)".*/\1/p' \
                  ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json)
                project_core=$(sed -n 's/.*"project_core_source_sha256":"\([0-9a-f]*\)".*/\1/p' \
                  ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json)
                composite_source=$(sed -n 's/.*"composite_artifact_source_sha256":"\([0-9a-f]*\)".*/\1/p' \
                  ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json)
                test "''${#source_identity}" -eq 64
                test "''${#project_core}" -eq 64
                test "''${#composite_source}" -eq 64
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
                  --subst-var-by source_identity "$source_identity" \
                  --subst-var-by project_core "$project_core" \
                  --subst-var-by composite_source "$composite_source" \
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

          mt7921-full-firmware-inert-proof-root-entry = pkgs.runCommand
            "mt7921-full-firmware-inert-proof-root-entry"
            { nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]; meta.mainProgram = "mt7921-full-firmware-inert-proof-root"; } ''
              mkdir -p "$out/bin" "$out/share/mt7921-full-firmware-inert-proof-root"
              runner=${mt7921-full-firmware-inert-proof}/bin/mt7921-full-firmware-inert-proof
              manifest=$out/share/mt7921-full-firmware-inert-proof-root/manifest
              cat > "$manifest" <<EOF
              RUNNER=$runner
              FLAVOR=full-firmware-production
              SOURCE_IDENTITY_SHA256=${mt7921FuchsiaSource.sourceIdentitySha256}
              EOF
              substitute ${./nix/mt7921-full-firmware-inert-proof-root.sh} \
                "$out/bin/mt7921-full-firmware-inert-proof-root" \
                --subst-var-by shell ${pkgs.runtimeShell} \
                --subst-var-by sudo /run/wrappers/bin/sudo \
                --subst-var-by runner "$runner" \
                --subst-var-by runner_package ${mt7921-full-firmware-inert-proof} \
                --subst-var-by manifest "$manifest" \
                --subst-var-by launcher ${mt7921-full-firmware-validation}/bin/mt7921-full-firmware-validation \
                --subst-var-by artifact_identity ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json \
                --subst-var-by source_identity ${mt7921FuchsiaSource.sourceIdentitySha256} \
                --subst-var-by runner_sha256 unused \
                --subst-var-by runner_registered_hash unused \
                --subst-var-by fuchsia_base_revision ${mt7921FuchsiaSource.fuchsiaBaseRevision} \
                --subst-var-by fuchsia_patch_set ${mt7921FuchsiaSource.fuchsiaOrderedPatchSetSha256} \
                --subst-var-by materialized_tree unused \
                --subst-var-by generated_source unused \
                --subst-var-by sha256sum ${pkgs.coreutils}/bin/sha256sum \
                --subst-var-by grep ${pkgs.gnugrep}/bin/grep \
                --subst-var-by cat ${pkgs.coreutils}/bin/cat
              chmod +x "$out/bin/mt7921-full-firmware-inert-proof-root"
              ${pkgs.bash}/bin/bash -n "$out/bin/mt7921-full-firmware-inert-proof-root"
            '';

          mt7921-full-firmware-inert-proof-root-entry-test = pkgs.runCommand
            "mt7921-full-firmware-inert-proof-root-entry-test" { nativeBuildInputs = [ pkgs.gnugrep ]; } ''
              root=${mt7921-full-firmware-inert-proof-root-entry}/bin/mt7921-full-firmware-inert-proof-root
              grep -F '"artifact_identity":"mt7921-driver-v11"' \
                ${mt7921-full-firmware-validation}/share/mt7921-full-firmware-validation/artifact-identity.json
              grep -F 'BSS_WIRE_CONTRACT=connac2-bss-wire-v1' ${mt7921-full-firmware-validation-manifest}
              test -x "$root"
              touch "$out"
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
