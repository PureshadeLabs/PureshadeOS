ROOTFS_BIN := rootfs/lth/bin

# Output dir: workspace-root target/ (cargo always writes here regardless of cwd)
OROS_OUT := target/x86_64-oros/release

KERNEL_DEBUG   := target/x86_64-lythos/debug/lythos
KERNEL_RELEASE := target/x86_64-lythos/release/lythos

QEMU := qemu-system-x86_64
QEMU_FLAGS := -serial stdio -display none \
              -drive file=disk.img,format=raw,if=none,id=hd0 \
              -device virtio-blk-pci,drive=hd0 \
              -netdev user,id=net0 \
              -device virtio-net-pci,netdev=net0

.PHONY: all oros kernel run run-release run-gui debug clean

all: oros kernel

## Build all userspace binaries.
## Root .cargo/config.toml provides [target.x86_64-oros] rustflags.
## -Z flags and --target are passed explicitly so host tools are unaffected.
OROS_PKGS := -p lythd -p lythdist -p lythmsg -p lysh -p rutils -p rkilo -p rpkg
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
	cp $(OROS_OUT)/lysh      $(ROOTFS_BIN)/lysh
	cp $(OROS_OUT)/rutils    $(ROOTFS_BIN)/rutils
	cp $(OROS_OUT)/rkilo     $(ROOTFS_BIN)/rkilo
	cp $(OROS_OUT)/rpkg      $(ROOTFS_BIN)/rpkg
	cp $(OROS_OUT)/lythd     rootfs/lth/system/init

KERNEL_FLAGS := --target targets/x86_64-lythos.json \
                -Z build-std=core,alloc,compiler_builtins \
                -Z build-std-features=compiler-builtins-mem \
                -Z json-target-spec

## Build the kernel (build.rs also runs mkrfs → disk.img)
kernel:
	cargo +nightly build -q $(KERNEL_FLAGS) -p lythos

kernel-release:
	cargo +nightly build --release -q $(KERNEL_FLAGS) -p lythos

## Run debug kernel under QEMU
run: kernel
	$(QEMU) -kernel $(KERNEL_DEBUG) $(QEMU_FLAGS)

## Run release kernel under QEMU
run-release: kernel-release
	$(QEMU) -kernel $(KERNEL_RELEASE) $(QEMU_FLAGS)

## Run with graphical display
run-gui: kernel
	$(QEMU) -kernel $(KERNEL_DEBUG) \
	        -serial stdio \
	        -drive file=disk.img,format=raw,if=none,id=hd0 \
	        -device virtio-blk-pci,drive=hd0 \
	        -netdev user,id=net0 \
	        -device virtio-net-pci,netdev=net0 \
	        -vga std

## Run with interrupt/triple-fault tracing
debug: kernel
	$(QEMU) -kernel $(KERNEL_DEBUG) $(QEMU_FLAGS) -d int,cpu_reset

clean:
	cargo clean
	rm -f disk.img
