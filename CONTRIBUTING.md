# Contributing to TIME Coin Protocol

Thank you for your interest in contributing to TIME Coin! This document provides guidelines for contributing to the project.

## Code of Conduct

- Be respectful and inclusive
- Focus on constructive feedback
- Help others learn and grow
- Follow the project's coding standards

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/yourusername/timecoin.git`
3. Create a branch: `git checkout -b feature/your-feature`
4. Make your changes
5. Test your changes: `cargo test && cargo clippy`
6. Format your code: `cargo fmt`
7. Commit: `git commit -m "Description of changes"`
8. Push: `git push origin feature/your-feature`
9. Create a Pull Request

## Development Guidelines

### Rust Style

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Use `cargo fmt` before committing
- Ensure `cargo clippy` passes without warnings
- Write tests for new functionality

### Commit Messages

- Use clear, descriptive commit messages
- Start with a verb (Add, Fix, Update, Remove, etc.)
- Reference issues when applicable: `Fix #123: Description`

### Testing

- Add unit tests for new functions
- Add integration tests for new features
- Ensure all tests pass: `cargo test`
- Test on both mainnet and testnet configurations

### Documentation

- Document public APIs with doc comments (`///`)
- Update README.md if adding user-facing features
- Add examples for complex functionality

## Pull Request Process

1. Ensure your code builds and all tests pass
2. Update documentation as needed
3. Describe your changes clearly in the PR description
4. Link any related issues
5. Wait for review and address feedback
6. Once approved, a maintainer will merge your PR

## Reporting Bugs

Use GitHub Issues to report bugs. Include:

- Clear description of the issue
- Steps to reproduce
- Expected vs actual behavior
- Environment details (OS, Rust version, etc.)
- Logs or error messages

## Feature Requests

We welcome feature requests! Please:

- Check existing issues first
- Describe the feature clearly
- Explain the use case
- Consider implementation approach

## Questions?

- Join our Discord server
- Open a GitHub Discussion
- Email: dev@time-coin.io

Thank you for contributing! 🎉
