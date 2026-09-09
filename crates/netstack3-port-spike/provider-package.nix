{
  lib,
  rustPlatform,
  fetchzip,
  runCommand,
  python3,
}:

let
  reference = builtins.fromJSON (builtins.readFile ../../nix/fuchsia-reference.json);
  inherit (reference) commit;
  archives = map (item: item // {
    src = fetchzip {
      url = "https://fuchsia.googlesource.com/fuchsia/+archive/${commit}/${item.path}.tar.gz";
      inherit (item) hash;
      stripRoot = false;
    };
  }) reference.providerArchives;
  source = runCommand "netstack3-provider-source-${commit}" {
    nativeBuildInputs = [ python3 ];
  } ''
    refroot="$out/reference/fuchsia-${commit}"
    mkdir -p "$refroot"
    ${lib.concatMapStringsSep "\n" (item: ''
      mkdir -p "$refroot/${item.path}"
      cp -R ${item.src}/. "$refroot/${item.path}/"
    '') archives}
    chmod -R u+w "$out"

    cp -R ${./upstream-cargo}/src/. "$refroot/src/"
    cp -R ${../directory-capability} "$refroot/src/lib/directory-capability"
    cp -R ${./upstream-cargo}/third_party/. "$refroot/third_party/"
    cp ${./upstream-cargo}/LICENSE.fuchsia "$refroot/LICENSE"
    cat >"$out/Cargo.toml" <<'CARGO'
    [workspace]
    members = ["crates/netstack3-port-spike"]
    resolver = "3"

    [workspace.package]
    edition = "2024"
    license = "MIT OR Apache-2.0"
    version = "0.1.0"
    CARGO
    mkdir -p "$out/crates/netstack3-port-spike"
    cp ${./Cargo.toml} "$out/crates/netstack3-port-spike/Cargo.toml"
    cp -R ${./src} "$out/crates/netstack3-port-spike/src"
    chmod -R u+w "$out"
    sed -i '/\[package\]/a workspace = "../../connectivity/network/netstack3"' "$refroot/src/lib/directory-capability/Cargo.toml"

    patch -d "$refroot" -p1 < ${./upstream-cargo/patches/dhcp-client-core-host.patch}
    patch -d "$refroot" -p1 < ${./upstream-cargo/patches/trust-dns-workspace.patch}

    # Cargo otherwise loads every package in the shared WLAN/Netstack3
    # development workspace. The deployment build retains the one canonical
    # manifest and lock while selecting only the daemon's source closure.
    python3 - "$refroot/src/connectivity/network/netstack3/Cargo.toml" <<'PY'
    from pathlib import Path
    import sys

    manifest = Path(sys.argv[1])
    text = manifest.read_text()
    start = text.index("members = [")
    end = text.index("\n]\n\n[workspace.package]", start) + 2
    members = """members = [
      "cargo/net-types-macros", "cargo/net-declare-macros", "cargo/net-declare-portable", "cargo/port-integration",
      "../dhcpv4/client/core", "../dhcpv4/protocol",
      "../../../../third_party/rust_crates/forks/trust-dns-proto-0.22.0",
      "../../../../third_party/rust_crates/forks/trust-dns-resolver-0.22.0",
      "../../lib/net-types", "../../lib/internet-checksum", "../../lib/packet-formats",
      "../lib/diagnostics-traits", "../lib/explicit", "../../../lib/network/packet",
      "../../../lib/replace-with", "core", "core/base", "core/datagram", "core/device",
      "core/filter", "core/hashmap", "core/icmp_echo", "core/ip", "core/lock-order",
      "core/macros", "core/sync", "core/tcp", "core/trace", "core/udp",
    ]"""
    manifest.write_text(text[:start] + members + text[end:])
    PY
  '';
in
rustPlatform.buildRustPackage {
  pname = "netstack3-provider-daemon";
  version = "0.1.0";
  src = source;
  cargoRoot = "reference/fuchsia-${commit}/src/connectivity/network/netstack3";
  buildAndTestSubdir = "reference/fuchsia-${commit}/src/connectivity/network/netstack3";
  cargoLock.lockFile = ./upstream-cargo/src/connectivity/network/netstack3/Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "netstack3-port-integration"
    "--bins"
  ];
  doCheck = false;

  meta = {
    description = "Native Netstack3 Linux provider daemon and link supervisor";
    license = with lib.licenses; [
      bsd2
      mit
      asl20
    ];
    mainProgram = "netstack3-provider-daemon";
    platforms = lib.platforms.linux;
  };
}
