{
  stdenv,
  testers,
  writeText,
  rustPlatform,
}:

let
  vfioProbeSource = writeText "vfio-iommufd-probe.c" ''
    #include <errno.h>
    #include <fcntl.h>
    #include <linux/iommufd.h>
    #include <linux/vfio.h>
    #include <stdint.h>
    #include <stdio.h>
    #include <stdlib.h>
    #include <sys/ioctl.h>
    #include <sys/mman.h>
    #include <unistd.h>

    static void fail(const char *operation) {
      perror(operation);
      exit(EXIT_FAILURE);
    }

    int main(int argc, char **argv) {
      if (argc != 2) {
        fprintf(stderr, "usage: %s /dev/vfio/devices/vfioN\n", argv[0]);
        return EXIT_FAILURE;
      }

      int device = open(argv[1], O_RDWR);
      if (device < 0) fail("open VFIO device cdev");
      int iommufd = open("/dev/iommu", O_RDWR);
      if (iommufd < 0) fail("open /dev/iommu");

      struct vfio_device_bind_iommufd bind = {
        .argsz = sizeof(bind),
        .iommufd = iommufd,
      };
      if (ioctl(device, VFIO_DEVICE_BIND_IOMMUFD, &bind))
        fail("VFIO_DEVICE_BIND_IOMMUFD");

      struct iommu_ioas_alloc alloc = { .size = sizeof(alloc) };
      if (ioctl(iommufd, IOMMU_IOAS_ALLOC, &alloc)) fail("IOMMU_IOAS_ALLOC");

      struct vfio_device_attach_iommufd_pt attach = {
        .argsz = sizeof(attach),
        .pt_id = alloc.out_ioas_id,
      };
      if (ioctl(device, VFIO_DEVICE_ATTACH_IOMMUFD_PT, &attach))
        fail("VFIO_DEVICE_ATTACH_IOMMUFD_PT");

      struct vfio_device_info info = { .argsz = sizeof(info) };
      if (ioctl(device, VFIO_DEVICE_GET_INFO, &info)) fail("VFIO_DEVICE_GET_INFO");
      if (!(info.flags & VFIO_DEVICE_FLAGS_PCI) || !info.num_regions || !info.num_irqs) {
        fprintf(stderr, "unexpected VFIO PCI capabilities: flags=%#x regions=%u irqs=%u\n",
                info.flags, info.num_regions, info.num_irqs);
        return EXIT_FAILURE;
      }

      unsigned usable_regions = 0;
      for (unsigned index = 0; index < info.num_regions; index++) {
        struct vfio_region_info region = {
          .argsz = sizeof(region),
          .index = index,
        };
        if (ioctl(device, VFIO_DEVICE_GET_REGION_INFO, &region)) {
          if (errno == EINVAL) continue;
          fail("VFIO_DEVICE_GET_REGION_INFO");
        }
        usable_regions += region.size != 0;
      }

      unsigned usable_irqs = 0;
      for (unsigned index = 0; index < info.num_irqs; index++) {
        struct vfio_irq_info irq = {
          .argsz = sizeof(irq),
          .index = index,
        };
        if (ioctl(device, VFIO_DEVICE_GET_IRQ_INFO, &irq)) {
          if (errno == EINVAL) continue;
          fail("VFIO_DEVICE_GET_IRQ_INFO");
        }
        usable_irqs += irq.count != 0;
      }
      if (!usable_regions || !usable_irqs) {
        fprintf(stderr, "VFIO device has no usable region or IRQ\n");
        return EXIT_FAILURE;
      }

      long page_size = sysconf(_SC_PAGESIZE);
      void *page = mmap(NULL, page_size, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
      if (page == MAP_FAILED) fail("mmap DMA page");
      struct iommu_ioas_map map = {
        .size = sizeof(map),
        .flags = IOMMU_IOAS_MAP_READABLE | IOMMU_IOAS_MAP_WRITEABLE,
        .ioas_id = alloc.out_ioas_id,
        .user_va = (uintptr_t)page,
        .length = page_size,
      };
      if (ioctl(iommufd, IOMMU_IOAS_MAP, &map)) fail("IOMMU_IOAS_MAP");

      printf("VFIO PCI device: %u regions (%u usable), %u IRQs (%u usable); "
             "mapped DMA page at IOVA %#llx\n",
             info.num_regions, usable_regions, info.num_irqs, usable_irqs,
             (unsigned long long)map.iova);

      struct iommu_ioas_unmap unmap = {
        .size = sizeof(unmap),
        .ioas_id = alloc.out_ioas_id,
        .iova = map.iova,
        .length = map.length,
      };
      if (ioctl(iommufd, IOMMU_IOAS_UNMAP, &unmap)) fail("IOMMU_IOAS_UNMAP");
      close(device); /* Detaches and unbinds from iommufd. */
      struct iommu_destroy destroy = {
        .size = sizeof(destroy),
        .id = alloc.out_ioas_id,
      };
      if (ioctl(iommufd, IOMMU_DESTROY, &destroy)) fail("IOMMU_DESTROY");
      close(iommufd);
      munmap(page, page_size);
      return EXIT_SUCCESS;
    }
  '';

  vfioProbe = stdenv.mkDerivation {
    pname = "vfio-iommufd-probe";
    version = "1";
    dontUnpack = true;
    buildPhase = ''
      $CC -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror \
        ${vfioProbeSource} -o vfio-iommufd-probe
    '';
    installPhase = ''
      install -Dm755 vfio-iommufd-probe $out/bin/vfio-iommufd-probe
    '';
  };
  safeVfioEdu = rustPlatform.buildRustPackage {
    pname = "safe-vfio-edu";
    version = "0.1.0";
    # `builtins.path` includes new workspace members before the jj commit exists.
    src = builtins.path { path = ../.; name = "drv-source"; };
    cargoLock.lockFile = ../Cargo.lock;
    cargoBuildFlags = [ "-p" "drv-hardware-backends" "--bin" "vfio_edu" ];
    cargoTestFlags = [ "-p" "drv-hardware-backends" ];
  };
in
testers.runNixOSTest {
  name = "vfio-edu";

  nodes.machine = { pkgs, ... }: {
    boot.kernelParams = [
      "intel_iommu=on"
      "iommu.strict=1"
    ];
    boot.kernelModules = [
      "vfio-pci"
      "iommufd"
    ];

    environment.systemPackages = [
      pkgs.pciutils
      vfioProbe
      safeVfioEdu
    ];

    virtualisation.memorySize = 1024;
    virtualisation.qemu.options = [
      "-machine q35,kernel_irqchip=split"
      "-device intel-iommu,intremap=on,caching-mode=on"
      "-device edu,id=edu"
    ];
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")

    with subtest("QEMU edu function is enumerated behind the virtual IOMMU"):
        bdf = machine.succeed(
            "for d in /sys/bus/pci/devices/*; do "
            "[ \"$(cat $d/vendor)\" = 0x1234 ] && "
            "[ \"$(cat $d/device)\" = 0x11e8 ] && basename $d; done; true"
        ).strip()
        assert bdf, "QEMU edu PCI function 1234:11e8 was not found"
        machine.succeed(f"lspci -s {bdf} -nn | grep -F '[1234:11e8]'")
        machine.succeed(f"test -L /sys/bus/pci/devices/{bdf}/iommu_group")
        group = machine.succeed(
            f"basename $(readlink -f /sys/bus/pci/devices/{bdf}/iommu_group)"
        ).strip()
        members = machine.succeed(
            f"find /sys/kernel/iommu_groups/{group}/devices -mindepth 1 -maxdepth 1 -printf '%f\\n'"
        ).split()
        assert members == [bdf], f"edu IOMMU group is not exclusive: {members}"

    with subtest("the complete group binds exclusively to vfio-pci"):
        machine.succeed("modprobe vfio-pci")
        machine.succeed(f"echo vfio-pci > /sys/bus/pci/devices/{bdf}/driver_override")
        machine.execute(
            f"test ! -L /sys/bus/pci/devices/{bdf}/driver || "
            f"echo {bdf} > /sys/bus/pci/devices/{bdf}/driver/unbind"
        )
        machine.succeed(f"echo {bdf} > /sys/bus/pci/drivers/vfio-pci/bind")
        machine.succeed(
            f"test \"$(basename $(readlink -f /sys/bus/pci/devices/{bdf}/driver))\" = vfio-pci"
        )
        for member in members:
            machine.succeed(
                f"test \"$(basename $(readlink -f /sys/bus/pci/devices/{member}/driver))\" = vfio-pci"
            )

    with subtest("modern VFIO cdev and iommufd provide region, IRQ, and DMA APIs"):
        machine.succeed("test -c /dev/iommu")
        cdev = machine.succeed(
            f"basename /sys/bus/pci/devices/{bdf}/vfio-dev/vfio*"
        ).strip()
        machine.succeed(f"test -c /dev/vfio/devices/{cdev}")
        machine.succeed(f"vfio-iommufd-probe /dev/vfio/devices/{cdev}")

    with subtest("safe Rust capability API drives QEMU edu through VFIO/iommufd"):
        machine.succeed(f"vfio_edu /dev/vfio/devices/{cdev}")

    with subtest("VFIO ownership can be torn down and restored"):
        machine.succeed(f"echo {bdf} > /sys/bus/pci/drivers/vfio-pci/unbind")
        machine.succeed(f"test ! -L /sys/bus/pci/devices/{bdf}/driver")
        machine.succeed(f"echo {bdf} > /sys/bus/pci/drivers/vfio-pci/bind")
        machine.succeed(
            f"test \"$(basename $(readlink -f /sys/bus/pci/devices/{bdf}/driver))\" = vfio-pci"
        )

    machine.shutdown()
  '';
}
