TARGET   := riscv32imac-unknown-none-elf
ELF_DEV  := target/$(TARGET)/debug/embed-os
ELF_REL  := target/$(TARGET)/release/embed-os
QEMU     := qemu-system-riscv32
QEMU_FLAGS := -machine virt -nographic -bios none

.PHONY: build release run run-release test disasm size nm clean

build:
	cargo build

release:
	cargo build --release

run: build
	@echo "tip: type 'poweroff' in the guest shell, or press Ctrl-A X to exit QEMU"
	$(QEMU) $(QEMU_FLAGS) -kernel $(ELF_DEV)

run-release: release
	$(QEMU) $(QEMU_FLAGS) -kernel $(ELF_REL)

test:
	./test.sh

disasm: build
	riscv64-unknown-elf-objdump -d $(ELF_DEV) | less

size: build
	riscv64-unknown-elf-size -A $(ELF_DEV) || llvm-size -m $(ELF_DEV)

nm: build
	riscv64-unknown-elf-nm -n $(ELF_DEV) | head -30

clean:
	cargo clean
	rm -f /tmp/embed-os-qemu-*.log
