"""Typed error classes for WeftCodec v1.

The interoperable contract is the class name, not the human-readable message.
Names follow ``spec/WEFTCODEC-FORMAT.md`` §11 exactly so a conformance vector's
``expect.error`` string can be compared directly with
``WeftError.error_class``.
"""

from __future__ import annotations


class WeftError(Exception):
    """Base class for every typed codec error."""

    error_class = "Malformed"

    def __init__(self, message: str = "") -> None:
        super().__init__(message)
        self.message = message


class Malformed(WeftError):
    """Structural or byte error, including bad varints and bad tables."""

    error_class = "Malformed"


class Corrupt(WeftError):
    """Checksum, hash, or length mismatch."""

    error_class = "Corrupt"


class UnsupportedVersion(WeftError):
    """Major/minor not supported."""

    error_class = "UnsupportedVersion"


class UnsupportedMode(WeftError):
    """Mode not implementable."""

    error_class = "UnsupportedMode"


class UnsupportedFeature(WeftError):
    """Unknown critical flag or feature."""

    error_class = "UnsupportedFeature"


class LimitExceeded(WeftError):
    """A configured resource bound would be exceeded."""

    error_class = "LimitExceeded"


class DependencyMissing(WeftError):
    """A dependency cannot be found."""

    error_class = "DependencyMissing"


class DependencyDenied(WeftError):
    """Authorization refuses the dependency."""

    error_class = "DependencyDenied"


class DependencyMismatch(WeftError):
    """Resolved bytes fail hash or length verification."""

    error_class = "DependencyMismatch"


class DependencyCycle(WeftError):
    """The reconstruction dependency graph is cyclic."""

    error_class = "DependencyCycle"


class NonCanonical(WeftError):
    """Structured bytes violate WCB canonical form."""

    error_class = "NonCanonical"


class Io(WeftError):
    """Transport or read failure."""

    error_class = "Io"


ERROR_CLASSES = {
    cls.error_class: cls
    for cls in (
        Malformed,
        Corrupt,
        UnsupportedVersion,
        UnsupportedMode,
        UnsupportedFeature,
        LimitExceeded,
        DependencyMissing,
        DependencyDenied,
        DependencyMismatch,
        DependencyCycle,
        NonCanonical,
        Io,
    )
}


def error_for_class(name: str, message: str = "") -> WeftError:
    """Build the typed exception for a normative class name."""
    try:
        cls = ERROR_CLASSES[name]
    except KeyError:
        raise ValueError("unknown Weft error class %r" % (name,))
    return cls(message)
