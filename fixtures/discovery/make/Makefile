# Fixture Makefile. Targets are read by name only; nothing here is evaluated.
BUILD_TIME := $(shell date +%s)
PORT ?= 8080
include config.mk

.PHONY: dev test build clean

dev: ## Run the development server
	go run ./cmd/server --port $(PORT)

test: build
	go test ./...

build: \
		go.mod
	go build -o bin/server ./cmd/server

clean:
	rm -rf bin

define HELP_TEXT
not-a-target: nope
endef
