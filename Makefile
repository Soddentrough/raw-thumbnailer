prefix ?= /usr/local
bindir ?= $(prefix)/bin
datadir ?= $(prefix)/share

build:
	cargo build --release

install: build
	install -D -m 755 target/release/raw-thumbnailer $(DESTDIR)$(bindir)/raw-thumbnailer
	install -D -m 644 raw-thumbnailer.thumbnailer $(DESTDIR)$(datadir)/thumbnailers/raw-thumbnailer.thumbnailer

uninstall:
	rm -f $(DESTDIR)$(bindir)/raw-thumbnailer
	rm -f $(DESTDIR)$(datadir)/thumbnailers/raw-thumbnailer.thumbnailer

clean:
	cargo clean
