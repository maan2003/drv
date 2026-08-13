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
              runHook postInstallCheck
            '';
            meta.mainProgram = "mt7921-passive-scan";
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
