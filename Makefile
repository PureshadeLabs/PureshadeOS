ROOTFS_BIN := rootfs/lth/bin

# Output dir: workspace-root target/ (cargo always writes here regardless of cwd)
OROS_OUT := target/x86_64-oros/release
# release-closed profile writes to a separate target subdir (see Cargo.toml).
OROS_OUT_CLOSED := target/x86_64-oros/release-closed

KERNEL_DEBUG   := target/x86_64-lythos/debug/lythos
KERNEL_RELEASE := target/x86_64-lythos/release/lythos
KERNEL_CLOSED  := target/x86_64-lythos/release-closed/lythos

QEMU := qemu-system-x86_64
# Guest RAM. QEMU's 128 MiB default is too small for Limine 11's high-memory
# allocator to load the ~8 MB debug kernel (PANIC: High memory allocator OOM).
# Must stay ≤ 1 GiB: the kernel boot-loops above that (early-boot access
# outside the 1 GiB identity map — docs/plans/followup-code-tasks.md item 11).
QEMU_MEM ?= 512M

# ── Limine / OVMF ─────────────────────────────────────────────────────────────
# Auto-detected for common setups; override by setting in the environment.
#
#   NixOS (x86_64):  nix-shell -p limine ovmf mtools
#   nix on Darwin:   see below — QEMU ships EDK2; limine cross-built for x86_64
#   macOS (brew):    brew install mtools qemu
#                    OVMF from $(brew --prefix qemu)/share/qemu/edk2-x86_64-*.fd
#
# LIMINE_EFI: prefer limine in PATH (x86_64 build); fall back to nix store search.
LIMINE_EFI ?= $(shell \
  _l=$$(command -v limine 2>/dev/null); \
  if [ -n "$$_l" ]; then \
    _efi=$$(dirname "$$_l")/../share/limine/BOOTX64.EFI; \
    [ -f "$$_efi" ] && echo "$$_efi" && exit 0; \
  fi; \
  find /nix/store -maxdepth 5 -name 'BOOTX64.EFI' 2>/dev/null | head -1)

# OVMF_CODE / OVMF_VARS: check standard paths, then QEMU's bundled EDK2.
_QEMU_SHARE := $(shell _q=$$(command -v qemu-system-x86_64 2>/dev/null); [ -n "$$_q" ] && dirname $$(realpath "$$_q" 2>/dev/null || readlink -f "$$_q" 2>/dev/null || echo "$$_q"))/../share/qemu
OVMF_CODE  ?= $(shell \
  for p in /usr/share/OVMF/OVMF_CODE.fd \
            $(_QEMU_SHARE)/edk2-x86_64-code.fd \
            $$(brew --prefix qemu 2>/dev/null)/share/qemu/edk2-x86_64-code.fd; do \
    [ -f "$$p" ] && echo "$$p" && break; \
  done)
# x86_64 vars: use OVMF_VARS.fd if present; fall back to QEMU's i386 vars
# (same UEFI NVRAM format, works as blank writable NVRAM for x86_64 guests).
OVMF_VARS  ?= $(shell \
  for p in /usr/share/OVMF/OVMF_VARS.fd \
            $(_QEMU_SHARE)/edk2-x86_64-vars.fd \
            $(_QEMU_SHARE)/edk2-i386-vars.fd \
            $$(brew --prefix qemu 2>/dev/null)/share/qemu/edk2-x86_64-vars.fd; do \
    [ -f "$$p" ] && echo "$$p" && break; \
  done)

.PHONY: all oros kernel run run-probes run-release run-gui debug run-limine image run-iso-bios run-iso-uefi clean oros-closed kernel-closed closed

all: oros kernel

## Build all userspace binaries.
## Root .cargo/config.toml provides [target.x86_64-oros] rustflags.
## -Z flags and --target are passed explicitly so host tools are unaffected.
OROS_PKGS := -p lythd -p lythdist -p lythmsg -p netd -p lysh -p rutils -p rkilo -p shade
OROS_FLAGS := --target targets/x86_64-oros.json \
              -Z build-std=core,alloc,compiler_builtins \
              -Z build-std-features=compiler-builtins-mem \
              -Z json-target-spec

oros:
	cargo +nightly build --release -q $(OROS_FLAGS) $(OROS_PKGS)
	mkdir -p $(ROOTFS_BIN)
	cp $(OROS_OUT)/lythd     $(ROOTFS_BIN)/lythd
	cp $(OROS_OUT)/lythdist  $(ROOTFS_BIN)/lythdist
	cp $(OROS_OUT)/lythmsg   $(ROOTFS_BIN)/lythmsg
	cp $(OROS_OUT)/netd      $(ROOTFS_BIN)/netd
	cp $(OROS_OUT)/lysh      $(ROOTFS_BIN)/lysh
	cp $(OROS_OUT)/rutils    $(ROOTFS_BIN)/rutils
	cp $(OROS_OUT)/rkilo     $(ROOTFS_BIN)/rkilo
	cp $(OROS_OUT)/shade      $(ROOTFS_BIN)/shade
	cp $(OROS_OUT)/lythd     rootfs/lth/system/init

## Closed-source build (obfuscated). Uses the release-closed profile (Cargo.toml)
## plus rustflags that a profile cannot set: strip absolute source paths, drop
## panic file/line detail, and drop Debug format strings. -Z flags are nightly;
## we already build with +nightly. Applied to both oros-closed and kernel-closed.
CLOSED_RUSTFLAGS := --remap-path-prefix $(CURDIR)= \
                    -Zlocation-detail=none \
                    -Zfmt-debug=none

oros-closed:
	RUSTFLAGS="$(CLOSED_RUSTFLAGS)" \
	  cargo +nightly build --profile release-closed -q $(OROS_FLAGS) $(OROS_PKGS)
	mkdir -p $(ROOTFS_BIN)
	cp $(OROS_OUT_CLOSED)/lythd     $(ROOTFS_BIN)/lythd
	cp $(OROS_OUT_CLOSED)/lythdist  $(ROOTFS_BIN)/lythdist
	cp $(OROS_OUT_CLOSED)/lythmsg   $(ROOTFS_BIN)/lythmsg
	cp $(OROS_OUT_CLOSED)/lysh      $(ROOTFS_BIN)/lysh
	cp $(OROS_OUT_CLOSED)/rutils    $(ROOTFS_BIN)/rutils
	cp $(OROS_OUT_CLOSED)/rkilo     $(ROOTFS_BIN)/rkilo
	cp $(OROS_OUT_CLOSED)/shade     $(ROOTFS_BIN)/shade
	cp $(OROS_OUT_CLOSED)/lythd     rootfs/lth/system/init

KERNEL_FLAGS := --target targets/x86_64-lythos.json \
                -Z build-std=core,alloc,compiler_builtins \
                -Z build-std-features=compiler-builtins-mem \
                -Z json-target-spec

## Build the kernel ELF (Limine-bootable; build.rs also runs mkrfs → disk.img)
kernel:
	cargo +nightly build -q $(KERNEL_FLAGS) -p lythos

## Kernel with the full boot test suite (userspace-entry/ELF/integration/sweep
## probes run in kmain — adds seconds to every boot; use for regression runs).
kernel-tests:
	cargo +nightly build -q $(KERNEL_FLAGS) -p lythos --features boot-tests

kernel-release:
	cargo +nightly build --release -q $(KERNEL_FLAGS) -p lythos

## Kernel, obfuscated closed-source build (release-closed profile + rustflags).
kernel-closed:
	RUSTFLAGS="$(CLOSED_RUSTFLAGS)" \
	  cargo +nightly build --profile release-closed -q $(KERNEL_FLAGS) -p lythos

## Full obfuscated build: userspace → rootfs/lth/bin/, then kernel.
closed: oros-closed kernel-closed

## Build a minimal UEFI boot image containing Limine + kernel + config.
## Requires: mtools (mformat + mcopy), Limine EFI binary, OVMF firmware.
##   NixOS: nix-shell -p limine ovmf mtools
##   Set LIMINE_EFI, OVMF_CODE, OVMF_VARS if not in default locations.
limine.img: $(KERNEL_DEBUG) limine.conf
	@command -v mformat >/dev/null 2>&1 || { \
	  echo "error: mtools not found. Install: nix-shell -p mtools  (NixOS)"; \
	  echo "                                   brew install mtools   (macOS)"; \
	  exit 1; }
	@test -f "$(LIMINE_EFI)" || { \
	  echo "error: Limine EFI binary not found at '$(LIMINE_EFI)'"; \
	  echo "       Set LIMINE_EFI=/path/to/BOOTX64.EFI or install limine."; \
	  exit 1; }
	dd if=/dev/zero of=$@ bs=512 count=131072 2>/dev/null
	mformat -i $@ -F -v BOOT ::
	mmd    -i $@ ::/EFI ::/EFI/BOOT
	mcopy  -i $@ $(LIMINE_EFI) ::/EFI/BOOT/BOOTX64.EFI
	mcopy  -i $@ $(KERNEL_DEBUG) ::/lythos.elf
	mcopy  -i $@ limine.conf ::/limine.conf
	@echo "[limine.img] boot image ready → $@"

## Run debug kernel under QEMU (Limine UEFI boot).
## QEMU 6+ Q35 auto-loads EDK2 firmware; no explicit OVMF args needed.
## Override: make run OVMF_CODE=/path/to/code.fd OVMF_VARS=/path/to/vars.fd
run: kernel limine.img
	@./run-limine.sh $(OVMF_CODE) $(OVMF_VARS)

run-limine: run

## Like `make run`, but builds the kernel with --features boot-tests so the
## in-kmain integration/probe suite (core_smoke: exit-record retention, etc.)
## executes during boot. Serial output shows `[integration] … passed` lines; a
## failed probe panics the kernel with the assert message. Prereq order matters
## (kernel-tests before limine.img, matching `run: kernel limine.img`): the
## boot-tests ELF is built first, then repacked into the boot image.
run-probes: kernel-tests limine.img
	@./run-limine.sh $(OVMF_CODE) $(OVMF_VARS)

## Run release kernel under QEMU (Limine UEFI boot).
run-release: kernel-release
	@KERNEL=$(KERNEL_RELEASE) ./run-limine.sh $(OVMF_CODE) $(OVMF_VARS)

## Persistent /shade/store backing image. A fixed, blank raw disk created ONCE
## and never regenerated by the build (build.rs must never truncate, overwrite,
## or recreate it — that would silently erase the store and defeat cold-boot
## persistence). The kernel formats RFS2 onto it lazily at the first store
## mount; every later boot mounts the existing volume. `make clean` does NOT
## remove it — use `make clean-store` to wipe it deliberately.
STORE_IMG      ?= store.img
STORE_IMG_SIZE ?= 64M

$(STORE_IMG):
	@echo "[store.img] creating blank persistent store image ($(STORE_IMG_SIZE))"
	@qemu-img create -f raw $@ $(STORE_IMG_SIZE) >/dev/null 2>&1 || truncate -s $(STORE_IMG_SIZE) $@

.PHONY: clean-store
## Deliberately wipe the persistent store (forces a fresh format next boot).
clean-store:
	rm -f $(STORE_IMG)

# ── Encrypted root (doc 08 — AES-256-GCM + Argon2id FDE) ──────────────────────
DISK_IMG_SIZE ?= 64M

.PHONY: disk-enc
## Create an ENCRYPTED root disk.img (AES-256-GCM block encryption, Argon2id
## KDF) from rootfs/. Requires RFS2_PASSPHRASE in the environment — the same
## passphrase the kernel prompts for at cold boot. Builds the crypto-capable
## mkrfs2 via cargo (the bare-rustc build used by build.rs is plaintext-only).
## While RFS2_PASSPHRASE is set, build.rs preserves this image (never
## regenerates it plaintext); store.img is encrypted lazily in-kernel under the
## same passphrase. Boot with: RFS2_PASSPHRASE=... make run  (then type it in).
disk-enc: oros
	@test -n "$(RFS2_PASSPHRASE)" || { \
	  echo "error: set RFS2_PASSPHRASE=<passphrase> first (the boot passphrase)"; \
	  exit 1; }
	cargo build --release --manifest-path tools/mkrfs2/Cargo.toml
	RFS2_PASSPHRASE="$(RFS2_PASSPHRASE)" \
	  tools/mkrfs2/target/release/mkrfs2 disk.img $(DISK_IMG_SIZE) rootfs
	@echo "[disk-enc] encrypted root disk.img ready — cold boot requires the passphrase"

## Run with graphical display (Limine UEFI boot).
## OVMF must be loaded explicitly via pflash — QEMU does not auto-load EDK2
## (q35 defaults to SeaBIOS; limine.img is UEFI-only, so the result is a
## black screen with no serial output). Uses the auto-detected $(OVMF_CODE)/
## $(OVMF_VARS); vars are copied to /tmp because pflash needs them writable.
run-gui: kernel limine.img $(STORE_IMG)
	@test -n "$(OVMF_CODE)" || { \
	  echo "error: OVMF firmware not found — set OVMF_CODE=/path/to/code.fd"; \
	  exit 1; }
	@cp "$(if $(OVMF_VARS),$(OVMF_VARS),$(OVMF_CODE))" /tmp/lythos-ovmf-vars.fd
	@chmod 644 /tmp/lythos-ovmf-vars.fd
	$(QEMU) \
	    -machine q35,usb=on \
	    -m $(QEMU_MEM) \
	    -drive if=pflash,format=raw,unit=0,file=$(OVMF_CODE),readonly=on \
	    -drive if=pflash,format=raw,unit=1,file=/tmp/lythos-ovmf-vars.fd \
	    -drive file=limine.img,format=raw,if=none,id=esp \
	    -device usb-storage,drive=esp \
	    -drive file=disk.img,format=raw,if=none,id=hd0 \
	    -device virtio-blk-pci,drive=hd0 \
	    -drive file=$(STORE_IMG),format=raw,if=none,id=hd1 \
	    -device virtio-blk-pci,drive=hd1 \
	    -netdev user,id=net0 \
	    -device virtio-net-pci,netdev=net0 \
	    -serial stdio \
	    -vga virtio

## Run with interrupt/triple-fault tracing.
debug: kernel limine.img
	@EXTRA_QEMU_FLAGS="-d int,cpu_reset" ./run-limine.sh $(OVMF_CODE) $(OVMF_VARS)

# ── Hybrid boot image: ISO + dd-able USB, BIOS + UEFI ────────────────────────
# See docs/bringup/boot-image.md for USB flashing and QEMU test invocations.

LIMINE_VERSION := 11.0.0
LIMINE_BIN_DIR := target/limine-binary
LIMINE_BIN_URL := https://github.com/limine-bootloader/limine/archive/refs/tags/v$(LIMINE_VERSION)-binary.tar.gz
ISO      := pureshadeos.iso
ISO_ROOT := target/iso_root

## Fetch the Limine binary release (BIOS stages + UEFI executables) and build
## the `limine` host tool (needed for bios-install). Cached under target/.
$(LIMINE_BIN_DIR)/limine:
	@echo "[limine] fetching binary release v$(LIMINE_VERSION)"
	curl -sfL --max-time 120 -o target/limine-binary.tar.gz $(LIMINE_BIN_URL)
	rm -rf $(LIMINE_BIN_DIR)
	mkdir -p target && tar xzf target/limine-binary.tar.gz -C target
	mv target/Limine-$(LIMINE_VERSION)-binary $(LIMINE_BIN_DIR)
	rm -f target/limine-binary.tar.gz
	cc -O2 -o $@ $(LIMINE_BIN_DIR)/limine.c

## Assemble the hybrid bootable ISO:
##   - BIOS boot: El Torito via limine-bios-cd.bin + limine-bios.sys, plus
##     `limine bios-install` so the same image boots from USB/HDD (MBR path).
##   - UEFI boot: El Torito EFI image (limine-uefi-cd.bin) + an ESP visible
##     when dd'd to USB (--efi-boot-part), holding /EFI/BOOT/BOOTX64.EFI.
## The kernel requests its framebuffer via Limine protocol request structs —
## no framebuffer directive is needed in limine.conf.
image: $(ISO)

$(ISO): kernel-release limine.conf $(LIMINE_BIN_DIR)/limine
	@command -v xorriso >/dev/null 2>&1 || { \
	  echo "error: xorriso not found. Install: brew install xorriso  (macOS)"; \
	  echo "                                   nix-shell -p xorriso   (NixOS)"; \
	  exit 1; }
	rm -rf $(ISO_ROOT)
	mkdir -p $(ISO_ROOT)/boot/limine $(ISO_ROOT)/EFI/BOOT
	cp $(KERNEL_RELEASE) $(ISO_ROOT)/lythos.elf
	# Distribution config: same entry as dev limine.conf, 3 s menu timeout.
	sed 's/^timeout: .*/timeout: 3/' limine.conf > $(ISO_ROOT)/limine.conf
	cp $(LIMINE_BIN_DIR)/limine-bios.sys     $(ISO_ROOT)/boot/limine/
	cp $(LIMINE_BIN_DIR)/limine-bios-cd.bin  $(ISO_ROOT)/boot/limine/
	cp $(LIMINE_BIN_DIR)/limine-uefi-cd.bin  $(ISO_ROOT)/boot/limine/
	cp $(LIMINE_BIN_DIR)/BOOTX64.EFI         $(ISO_ROOT)/EFI/BOOT/
	xorriso -as mkisofs \
	    -b boot/limine/limine-bios-cd.bin \
	    -no-emul-boot -boot-load-size 4 -boot-info-table \
	    --efi-boot boot/limine/limine-uefi-cd.bin \
	    -efi-boot-part --efi-boot-image --protective-msdos-label \
	    -volid PURESHADEOS \
	    $(ISO_ROOT) -o $@
	./$(LIMINE_BIN_DIR)/limine bios-install $@
	@echo "[image] hybrid BIOS+UEFI ISO ready → $@ (dd-able to USB)"

## Boot the ISO under SeaBIOS (legacy BIOS path).
run-iso-bios: $(ISO) $(STORE_IMG)
	$(QEMU) -machine q35 -m $(QEMU_MEM) -cdrom $(ISO) \
	    -drive file=disk.img,format=raw,if=none,id=hd0 \
	    -device virtio-blk-pci,drive=hd0 \
	    -drive file=$(STORE_IMG),format=raw,if=none,id=hd1 \
	    -device virtio-blk-pci,drive=hd1 \
	    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
	    -serial stdio -display none

## Boot the ISO under OVMF (UEFI path).
run-iso-uefi: $(ISO) $(STORE_IMG)
	cp $(OVMF_VARS) /tmp/pureshade-ovmf-vars.fd && chmod 644 /tmp/pureshade-ovmf-vars.fd
	$(QEMU) -machine q35 -m $(QEMU_MEM) \
	    -drive if=pflash,format=raw,unit=0,file=$(OVMF_CODE),readonly=on \
	    -drive if=pflash,format=raw,unit=1,file=/tmp/pureshade-ovmf-vars.fd \
	    -cdrom $(ISO) \
	    -drive file=disk.img,format=raw,if=none,id=hd0 \
	    -device virtio-blk-pci,drive=hd0 \
	    -drive file=$(STORE_IMG),format=raw,if=none,id=hd1 \
	    -device virtio-blk-pci,drive=hd1 \
	    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
	    -serial stdio -display none

clean:
	cargo clean
	# NOTE: store.img is deliberately NOT removed — it is the persistent
	# /shade/store backing (user data that must survive rebuilds and clean).
	# Use `make clean-store` to wipe it explicitly.
	rm -f disk.img limine.img $(ISO) /tmp/lythos-ovmf-vars.fd
