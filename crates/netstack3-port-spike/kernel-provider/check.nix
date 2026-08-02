{
  stdenv,
  fetchurl,
  bc,
  bison,
  elfutils,
  flex,
  openssl,
  pkg-config,
  zlib,
}:

stdenv.mkDerivation {
  pname = "netstack3-kernel-provider-check";
  version = "6.18.40";

  src = fetchurl {
    urls = [ "mirror://kernel/linux/kernel/v6.x/linux-6.18.40.tar.xz" ];
    hash = "sha256-NxL8Hsg55NqsmBF2yFGJEuj0UmUKrt/kOB2kQZYTpDE=";
  };
  patches = [ ./patches/0001-net-add-netstack3-userspace-socket-provider.patch ];

  nativeBuildInputs = [ bc bison elfutils flex openssl pkg-config zlib ];
  hardeningDisable = [ "all" ];
  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    export KBUILD_BUILD_TIMESTAMP=@0
    make defconfig
    scripts/config --file .config -e NETSTACK3_PROVIDER
    make olddefconfig
    make -j$NIX_BUILD_CORES \
      net/netstack3_provider.o net/ipv4/af_inet.o net/ipv6/af_inet6.o
    make -C tools/testing/selftests/netstack3
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/kernel/net/ipv4 $out/kernel/net/ipv6 $out/selftests
    cp net/netstack3_provider.o $out/kernel/net/
    cp net/ipv4/af_inet.o $out/kernel/net/ipv4/
    cp net/ipv6/af_inet6.o $out/kernel/net/ipv6/
    cp tools/testing/selftests/netstack3/provider $out/selftests/
    runHook postInstall
  '';
}
