# Contributing to TimeCoin

Thank you for your interest in contributing to TimeCoin! This document provides guidelines for contributing to the project.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/timecoin.git`
3. Create a feature branch: `git checkout -b feature/your-feature-name`
4. Make your changes
5. Test thoroughly
6. Commit with clear messages: `git commit -m "feat: add feature description"`
7. Push to your fork: `git push origin feature/your-feature-name`
8. Open a Pull Request

## Development Setup

See [QUICKSTART.md](QUICKSTART.md) for detailed setup instructions.

## Code Style

- Follow Rust best practices and idioms
- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Ensure `cargo test` passes
- Add tests for new functionality

## Commit Message Format

We follow conventional commits:

- `feat:` - New features
- `fix:` - Bug fixes
- `docs:` - Documentation changes
- `refactor:` - Code refactoring
- `test:` - Test additions/changes
- `chore:` - Maintenance tasks

## Pull Request Process

1. Update documentation for any user-facing changes
2. Add tests for new functionality
3. Ensure all tests pass
4. Update CHANGELOG.md if applicable
5. Request review from maintainers

## Code Review Guidelines

- Be respectful and constructive
- Focus on code quality and maintainability
- Test the changes locally when possible
- Ask questions if something is unclear

## Reporting Bugs

When reporting bugs, please include:

- TimeCoin version (`timed --version`)
- Operating system
- Steps to reproduce
- Expected vs actual behavior
- Relevant log output

## Feature Requests

Feature requests are welcome! Please:

- Check existing issues first
- Clearly describe the use case
- Explain why it benefits the TimeCoin network
- Consider implementation complexity

## Testing

- Write unit tests for new functions
- Add integration tests for new features
- Test edge cases and error conditions
- Ensure tests are deterministic

## Documentation

- Update relevant docs/ files
- Add inline code comments for complex logic
- Update README.md if needed
- Keep examples up-to-date

## License

By contributing, you agree that your contributions will be licensed under the Business Source License 1.1.

## Questions?

Feel free to open an issue for questions or join our community discussions.

---

Thank you for contributing to TimeCoin! 🚀
