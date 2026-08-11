import builtins as _builtins
import io
import json
import os
import socket
import sys
import traceback
import types


_SOCKET_ENV = "SWAGE_PYTHON_WORKER_SOCKET"
_ORIGINAL_OPEN = _builtins.open
_ORIGINAL_IMPORT = _builtins.__import__
_STDLIB_MODULE_NAMES = set(getattr(sys, "stdlib_module_names", ()))
_STDLIB_MODULE_NAMES.update(sys.builtin_module_names)
_CALL_COUNTER = 0


def _send(connection, payload):
    connection.sendall((json.dumps(payload) + "\n").encode("utf-8"))


def _recv(reader):
    line = reader.readline()
    if not line:
        raise RuntimeError("worker connection closed")
    return json.loads(line.decode("utf-8"))


def _next_call_id():
    global _CALL_COUNTER
    _CALL_COUNTER += 1
    return f"call_{_CALL_COUNTER}"


class ToolError(Exception):
    def __init__(self, tool_name, message, result=None):
        super().__init__(f"{tool_name}: {message}")
        self.tool_name = tool_name
        self.message = message
        self.result = result


def _dispatch_tool(connection, reader, request_id, tool_name, arguments):
    call_id = _next_call_id()
    _send(
        connection,
        {
            "type": "dispatch_tool",
            "request_id": request_id,
            "call_id": call_id,
            "tool_name": tool_name,
            "arguments": arguments,
        },
    )

    while True:
        message = _recv(reader)
        if message["type"] != "dispatch_tool_result":
            raise RuntimeError(f"unexpected host message: {message['type']}")
        if message["request_id"] != request_id or message["call_id"] != call_id:
            raise RuntimeError("dispatch_tool_result correlation mismatch")
        if message.get("error"):
            error = message["error"]
            raise ToolError(tool_name, error["message"], error.get("result"))
        return message.get("result")


def _make_python_execute_module(connection, reader, request_id):
    module = types.ModuleType("swage")
    module.ToolError = ToolError
    module.shell = lambda command: _dispatch_tool(
        connection, reader, request_id, "shell", {"command": command}
    )
    module.read_file = lambda path: _dispatch_tool(
        connection, reader, request_id, "file_read", {"path": path}
    )
    module.write_file = lambda path, content: _dispatch_tool(
        connection, reader, request_id, "file_write", {"path": path, "content": content}
    )
    module.edit_files = lambda changes: _dispatch_tool(
        connection, reader, request_id, "file_edit", {"changes": changes}
    )
    module.list_files = lambda path: _dispatch_tool(
        connection, reader, request_id, "file_list", {"path": path}
    )
    module.search_files = lambda pattern, path="": _dispatch_tool(
        connection,
        reader,
        request_id,
        "file_search",
        {"pattern": pattern, "path": path},
    )
    module.web_search = lambda query: _dispatch_tool(
        connection, reader, request_id, "web_search", {"query": query}
    )
    module.fetch_url = lambda url: _dispatch_tool(
        connection, reader, request_id, "url_fetch", {"url": url}
    )
    module.memory_read = lambda file=None: _dispatch_tool(
        connection,
        reader,
        request_id,
        "memory",
        {"operation": "read", **({"file": file} if file is not None else {})},
    )
    module.memory_write = lambda file, content: _dispatch_tool(
        connection,
        reader,
        request_id,
        "memory",
        {"operation": "write", "file": file, "content": content},
    )
    module.memory_list = lambda: _dispatch_tool(
        connection, reader, request_id, "memory", {"operation": "list"}
    )
    return module


def _restricted_builtins(allowed_imports):
    allowed = {
        "None": None,
        "True": True,
        "False": False,
        "abs": abs,
        "all": all,
        "any": any,
        "AssertionError": AssertionError,
        "bool": bool,
        "__build_class__": _builtins.__build_class__,
        "dict": dict,
        "enumerate": enumerate,
        "Exception": Exception,
        "float": float,
        "getattr": getattr,
        "hasattr": hasattr,
        "IndexError": IndexError,
        "int": int,
        "isinstance": isinstance,
        "KeyError": KeyError,
        "len": len,
        "list": list,
        "max": max,
        "min": min,
        "print": print,
        "range": range,
        "reversed": reversed,
        "RuntimeError": RuntimeError,
        "set": set,
        "sorted": sorted,
        "str": str,
        "sum": sum,
        "tuple": tuple,
        "TypeError": TypeError,
        "ValueError": ValueError,
        "zip": zip,
        "ZeroDivisionError": ZeroDivisionError,
    }

    def _safe_import(name, globals=None, locals=None, fromlist=(), level=0):
        if level != 0:
            raise ImportError("relative imports are not allowed")

        top_level_name = name.split(".", 1)[0]
        if top_level_name in allowed_imports:
            return allowed_imports[top_level_name]
        if top_level_name in _STDLIB_MODULE_NAMES:
            return _ORIGINAL_IMPORT(name, globals, locals, fromlist, level)
        raise ImportError(f"import of '{name}' is not allowed")

    allowed["__import__"] = _safe_import

    return allowed


def _execute_python(request_id, connection, reader, payload):
    module = _make_python_execute_module(connection, reader, request_id)
    stdout = io.StringIO()
    previous_stdout = sys.stdout
    previous_working_directory = os.getcwd()
    sys.stdout = stdout

    globals_dict = {
        "__builtins__": _restricted_builtins({"swage": module}),
        "__name__": "__main__",
        "swage": module,
    }

    try:
        # Run user code relative to the requested tool context directory so
        # imports and file access behave like the host-side invocation.
        os.chdir(payload["working_directory"])
        exec(compile(payload["code"], "<python_execute>", "exec"), globals_dict, globals_dict)
        return stdout.getvalue(), None, None, {"output": stdout.getvalue()}
    except BaseException as error:
        return stdout.getvalue(), error.__class__.__name__, traceback.format_exc(), None
    finally:
        os.chdir(previous_working_directory)
        sys.stdout = previous_stdout


def main():
    socket_path = os.environ.get(_SOCKET_ENV)
    if not socket_path:
        raise RuntimeError(f"{_SOCKET_ENV} is not set")

    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.connect(socket_path)
    reader = connection.makefile("rb")

    _send(connection, {"type": "ready"})

    message = _recv(reader)
    if message["type"] != "execute":
        raise RuntimeError(f"expected execute message, got {message['type']}")

    request_id = message["request_id"]
    payload = message["payload"]

    if payload["kind"] != "python_execute":
        raise RuntimeError(f"unknown execute payload kind: {payload['kind']}")
    stdout, exception_type, traceback_text, result = _execute_python(
        request_id, connection, reader, payload
    )

    _send(
        connection,
        {
            "type": "execution_result",
            "request_id": request_id,
            "stdout": stdout,
            "exception_type": exception_type,
            "traceback": traceback_text,
            "timed_out": False,
            "result": result,
        },
    )

    try:
        message = _recv(reader)
        if message["type"] == "shutdown":
            _send(connection, {"type": "shutdown_ack"})
    except Exception:
        pass


if __name__ == "__main__":
    try:
        main()
    except BaseException as error:
        socket_path = os.environ.get(_SOCKET_ENV)
        if socket_path:
            try:
                connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                connection.connect(socket_path)
                _send(
                    connection,
                    {
                        "type": "error",
                        "message": traceback.format_exc()
                        or f"{error.__class__.__name__}: {error}",
                    },
                )
            except Exception:
                pass
        sys.exit(1)
