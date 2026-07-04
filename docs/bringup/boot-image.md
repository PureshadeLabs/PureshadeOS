# Hybrid boot image — ISO + USB, BIOS + UEFI

`make image` produces `pureshadeos.iso`, a hybrid image bootable four ways:
CD/ISO or USB/HDD, under legacy BIOS or UEFI.

```bash
make image      # → pureshadeos.iso (release kernel)
```

## What the recipe does

1. Fetches the Limine v11.0.0 **binary release** into `target/limine-binary/`
   (BIOS stages, UEFI executables) and compiles the `limine` host tool from
   the bundled `limine.c` — needed for `bios-install`. Cached; `cargo clean`
   removes it, next `make image` re-fetches.
2. Stages `target/iso_root/`:

   ```
   lythos.elf                        release kernel
   limine.conf                       single Lythos entry, 3 s timeout
   boot/limine/limine-bios.sys       BIOS stage 3
   boot/limine/limine-bios-cd.bin    El Torito BIOS boot image
   boot/limine/limine-uefi-cd.bin    El Torito EFI boot image (FAT ESP)
   EFI/BOOT/BOOTX64.EFI              UEFI fallback path
   ```

   The kernel requests its framebuffer through Limine protocol request
   structs compiled into the binary — `limine.conf` needs no framebuffer
   directive. The dev config at the repo root uses `timeout: 0` for fast
   iteration; the ISO copy is rewritten to `timeout: 3` so real hardware
   gets a menu window.
3. `xorriso -as mkisofs` with both El Torito boot records
   (`-b limine-bios-cd.bin` for BIOS, `--efi-boot limine-uefi-cd.bin`) plus
   `-efi-boot-part --efi-boot-image --protective-msdos-label`, which exposes
   the EFI image as a real ESP when the ISO is written to a disk device.
4. `limine bios-install pureshadeos.iso` — installs Limine's BIOS stage 1/2
   into the image's MBR/GPT structures so the *same file* boots from USB or
   HDD on legacy BIOS (the El Torito record only covers optical boot).

## Flashing to USB

```bash
# macOS — find the device, unmount, raw-write
diskutil list                          # identify /dev/diskN (whole disk!)
diskutil unmountDisk /dev/diskN
sudo dd if=pureshadeos.iso of=/dev/rdiskN bs=4m status=progress
diskutil eject /dev/diskN

# Linux
sudo dd if=pureshadeos.iso of=/dev/sdX bs=4M status=progress conv=fsync
```

**`dd` overwrites the target device completely — double-check the device
node before running.** Use the whole-disk node (`/dev/rdiskN`, `/dev/sdX`),
not a partition.

## QEMU test invocations

Make targets (attach `disk.img` for the RFS root filesystem — the ISO holds
only the boot chain; without the virtio disk the kernel boots but has no
userspace to exec):

```bash
make run-iso-bios    # SeaBIOS (legacy) path
make run-iso-uefi    # OVMF (UEFI) path
```

Raw equivalents:

```bash
# UEFI (OVMF). QEMU ≥ 5.2 bundles EDK2 in its share dir; vars must be a
# writable copy.
Q=$(dirname $(readlink -f $(which qemu-system-x86_64)))/../share/qemu
cp $Q/edk2-i386-vars.fd /tmp/ovmf-vars.fd && chmod 644 /tmp/ovmf-vars.fd
qemu-system-x86_64 -machine q35 \
    -drive if=pflash,format=raw,unit=0,file=$Q/edk2-x86_64-code.fd,readonly=on \
    -drive if=pflash,format=raw,unit=1,file=/tmp/ovmf-vars.fd \
    -cdrom pureshadeos.iso \
    -drive file=disk.img,format=raw,if=none,id=hd0 -device virtio-blk-pci,drive=hd0 \
    -serial stdio

# Legacy BIOS (SeaBIOS — QEMU default, no pflash args)
qemu-system-x86_64 -machine q35 -cdrom pureshadeos.iso \
    -drive file=disk.img,format=raw,if=none,id=hd0 -device virtio-blk-pci,drive=hd0 \
    -serial stdio

# USB/HDD-boot simulation: present the ISO as a raw hard disk instead of a
# CD — exercises the MBR stages written by `limine bios-install`, the same
# path a dd'd USB stick uses on legacy-BIOS hardware.
qemu-system-x86_64 -machine q35 \
    -drive file=pureshadeos.iso,format=raw,if=none,id=iso0 \
    -device ide-hd,drive=iso0,bootindex=0 \
    -serial stdio
```

## Verified 2026-07-02

Both firmware paths boot the ISO to `lythos login:` under QEMU 10.2
(q35 machine), with the framebuffer console active:

| Firmware | Framebuffer | Result |
|----------|-------------|--------|
| OVMF (EDK2) | 1280×800 GOP, phys `0x80000000` | login prompt |
| SeaBIOS | 1280×800 VBE, phys `0xfd000000` | login prompt |
