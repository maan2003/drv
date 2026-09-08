{ fetchurl, gnutar, runCommand, xz }:
let
  tag = "v7.1.5";
  commit = "155b42bec9cbb6b8cdc47dd9bd09503a81fbe493";
  linux = fetchurl {
    url = "https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.1.5.tar.xz";
    hash = "sha256-IqAZazy83zTcJ7d1YfTQQFhf00R+3JqzUxoax54wQec=";
  };
in runCommand "mt76-reference-${tag}" {
  nativeBuildInputs = [ gnutar xz ];
  passthru = { referenceCommit = commit; referenceTag = tag; };
} ''
  root="$out/reference/linux-${tag}"
  mkdir -p "$root/drivers/net/wireless/mediatek"
  tar -xJf ${linux} --strip-components=5 -C \
    "$root/drivers/net/wireless/mediatek" linux-7.1.5/drivers/net/wireless/mediatek/mt76
  printf '%s\n' '${commit}' > "$root/COMMIT"
  printf '%s\n' '${tag}' > "$root/TAG"
  chmod -R a-w "$out"
''
