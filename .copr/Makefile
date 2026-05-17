PACKAGE := facelock
SPEC := dist/$(PACKAGE).spec
VERSION := $(shell awk '/^Version:/ {print $$2; exit}' $(SPEC))

srpm:
	dnf -y install rpm-build git
	mkdir -p $(HOME)/rpmbuild/SOURCES
	git archive --prefix=$(PACKAGE)-$(VERSION)/ --output=$(HOME)/rpmbuild/SOURCES/$(PACKAGE)-$(VERSION).tar.gz HEAD
	rpmbuild --define '_topdir $(HOME)/rpmbuild' --define '_srcrpmdir $(outdir)' -bs $(SPEC)
