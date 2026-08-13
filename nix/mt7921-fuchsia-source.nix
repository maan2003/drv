{
  lib,
  fetchzip,
  runCommand,
  gnutar,
  gzip,
  patch,
  coreutils,
  findutils,
  gnugrep,
  bash,
}:

let
  reference = builtins.fromJSON (builtins.readFile ./fuchsia-reference.json);
  inherit (reference) commit closure;
  archives = map (item: item // {
    src = fetchzip {
      url = "https://fuchsia.googlesource.com/fuchsia/+archive/${commit}/${item.path}.tar.gz";
      hash = item.hash;
      stripRoot = false;
    };
  }) reference.archives;
  boringssl = fetchzip {
    url = "https://boringssl.googlesource.com/boringssl/+archive/${reference.boringsslArchive.commit}.tar.gz";
    hash = reference.boringsslArchive.hash;
    stripRoot = false;
  };
  patchFiles = map (name:
    ../crates/netstack3-port-spike/upstream-cargo/patches/${name}
  ) [
    "dhcp-client-core-host.patch"
    "trust-dns-workspace.patch"
    "wlan-common-host.patch"
    "wlan-frame-writer-host.patch"
    "wlan-fcg-crypto-host.patch"
    "wlan-fidl-ext-host.patch"
    "wlan-rsn-host.patch"
    "wlan-sme-host.patch"
    "wlan-sme-provenance-host.patch"
    "wlancfg-selection-host.patch"
    "wlan-trace-host.patch"
    "wlan-timer-host.patch"
    "wlan-mlme-host.patch"
    "wlan-mlme-provenance-host.patch"
    "wlan-mlme-sae-disposition-host.patch"
    "wlan-mlme-sae-active-attempt-host.patch"
    "wlan-mlme-assoc-comeback-host.patch"
    "wlan-mlme-assoc-comeback-runtime-host.patch"
    "wlan-mlme-disconnect-fixed-telemetry-host.patch"
    "wlan-mlme-host-fixture.patch"
  ];
  patchRows = map (file: {
    name = builtins.baseNameOf file;
    sha256 = builtins.hashFile "sha256" file;
    path = file;
  }) patchFiles;
  orderedPatchManifest = lib.concatMapStrings
    (row: "${row.name}\t${row.sha256}\n") patchRows;
  orderedPatchSetSha256 = builtins.hashString "sha256" orderedPatchManifest;
  orderedPatchList = lib.concatStringsSep "," (map
    (row: "${row.name}:${row.sha256}") patchRows);
  projectSource = lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      let base = baseNameOf path;
      in base != ".jj" && base != ".git" && base != "reference" && base != "target";
  };
in
runCommand "mt7921-source-fuchsia-${commit}-${builtins.substring 0 12 orderedPatchSetSha256}" {
  nativeBuildInputs = [ gnutar gzip patch coreutils findutils gnugrep ];
  passthru = {
    fuchsiaBaseRevision = commit;
    fuchsiaSourceClosure = closure;
    fuchsiaOrderedPatchSetSha256 = orderedPatchSetSha256;
    fuchsiaOrderedPatchList = orderedPatchList;
    inherit orderedPatchManifest;
  };
} ''
  set -euo pipefail
  cp -R ${projectSource}/. "$out/"
  chmod -R u+w "$out"
  test ! -e "$out/reference"

  refroot="$out/reference/fuchsia-${commit}"
  mkdir -p "$refroot"
  ${lib.concatMapStringsSep "\n" (item: ''
    mkdir -p "$refroot/${item.path}"
    cp -R ${item.src}/. "$refroot/${item.path}/"
  '') archives}
  mkdir -p "$refroot/${reference.boringsslArchive.path}"
  cp -R ${boringssl}/. "$refroot/${reference.boringsslArchive.path}/"
  printf '%s\n' '${reference.boringsslArchive.commit}' > "$refroot/third_party/boringssl/COMMIT"
  printf '%s\n' '${commit}' > "$refroot/COMMIT"
  printf '%s\n' '${closure}' > "$refroot/.drv-source-closure"

  chmod -R u+w "$refroot"
  cp -R ${../crates/netstack3-port-spike/upstream-cargo}/src/. "$refroot/src/"
  cp -R ${../crates/netstack3-port-spike/upstream-cargo}/sdk/. "$refroot/sdk/"
  cp -R ${../crates/netstack3-port-spike/upstream-cargo}/third_party/. "$refroot/third_party/"
  cp ${../crates/netstack3-port-spike/upstream-cargo}/LICENSE.fuchsia "$refroot/LICENSE"
  chmod -R u+w "$refroot"

  cat > "$refroot/.drv-host-patches" <<'PATCHES'
${orderedPatchManifest}PATCHES
  printf '%s\n' '${orderedPatchSetSha256}' > "$refroot/.drv-host-patch-set"
  : > patch.log
  ${lib.concatMapStringsSep "\n" (row: ''
    printf 'applying %s %s\n' '${row.name}' '${row.sha256}' >> patch.log
    ${bash}/bin/bash ${./apply-exact-patch.sh} "$refroot" ${row.path} patch.log
  '') patchRows}

  generated_source_sha256=$(
    cd "$refroot"
    find . -type f \( -name '*.rs' -o -name Cargo.toml -o -name Cargo.lock \) -print0 | sort -z \
      | while IFS= read -r -d $'\0' file; do printf '%s\0' "$file"; sha256sum "$file"; done \
      | sha256sum | cut -d ' ' -f1
  )
  materialized_tree_sha256=$(
    cd "$refroot"
    find . \( -type f -o -type l \) \
      ! -name .drv-materialized-source-tree-sha256 \
      ! -name .drv-generated-crate-source-sha256 \
      ! -name .drv-source-identity-sha256 -print0 | sort -z \
      | while IFS= read -r -d $'\0' file; do
          printf '%s\0' "$file"
          if test -L "$file"; then readlink "$file"; else sha256sum "$file"; fi
        done | sha256sum | cut -d ' ' -f1
  )
  source_identity_sha256=$(
    printf '%s\n%s\n%s\n%s\n%s\n' '${commit}' '${closure}' \
      '${orderedPatchSetSha256}' "$materialized_tree_sha256" "$generated_source_sha256" \
      | sha256sum | cut -d ' ' -f1
  )
  printf '%s\n' "$materialized_tree_sha256" > "$refroot/.drv-materialized-source-tree-sha256"
  printf '%s\n' "$generated_source_sha256" > "$refroot/.drv-generated-crate-source-sha256"
  printf '%s\n' "$source_identity_sha256" > "$refroot/.drv-source-identity-sha256"
  if find "$refroot" -name '*.rej' -print | grep -q .; then
    find "$refroot" -name '*.rej' -print >&2
    exit 1
  fi

  cp "$refroot/.drv-host-patches" expected-patches
  ${bash}/bin/bash ${./verify-mt7921-fuchsia-source.sh} "$refroot" '${commit}' expected-patches \
    '${orderedPatchSetSha256}' '${closure}' derivation-owned

  mkdir -p "$out/share/mt7921-fuchsia-source"
  cp patch.log "$out/share/mt7921-fuchsia-source/patch.log"
  cat > "$out/share/mt7921-fuchsia-source/identity.json" <<IDENTITY
{"fuchsia_base_revision":"${commit}","fuchsia_source_closure":"${closure}","ordered_patch_set_sha256":"${orderedPatchSetSha256}","ordered_patch_list":"${orderedPatchList}","materialized_source_tree_sha256":"$materialized_tree_sha256","generated_crate_source_sha256":"$generated_source_sha256","source_identity_sha256":"$source_identity_sha256"}
IDENTITY
''
