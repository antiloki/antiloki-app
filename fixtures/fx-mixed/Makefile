# A MAKEFILE IS AN ENTRY POINT THE PROJECT DECLARED, and it is the branch gate_cmd reaches when a repo has no
# npm script for something. This one is here so that branch is exercised against a real file: a repo with two
# stacks usually answers "how do I test this" with make, not with either toolchain.
test:
	cargo test --quiet
	node --test web

lint:
	cargo clippy --quiet -- -D warnings
