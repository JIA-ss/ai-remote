"""Read the disposable native UI fixture's periodically rewritten state."""

import json
import time


def read_fixture_state(path, *, timeout=2.0, poll_interval=0.01):
    """Read a fresh complete snapshot, tolerating only a bounded producer write window."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, UnicodeDecodeError, json.JSONDecodeError) as error:
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"fixture state remained incomplete after {timeout:g}s ({type(error).__name__})"
                ) from error
            time.sleep(poll_interval)
