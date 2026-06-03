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


def _text_preview(messages):
    for message in messages:
        for part in message.get("content", []):
            if isinstance(part, dict) and part.get("type") == "text" and part.get("text"):
                return part["text"][:100]
    return None


class PyElement:
    def __init__(self, name, source, messages, token_count, priority):
        self.name = name
        self.source = source
        self.messages = list(messages)
        self.token_count = token_count
        self.priority = priority
        self.content_preview = _text_preview(self.messages)

    @property
    def element_name(self):
        return self.name

    @property
    def message_count(self):
        return len(self.messages)


class AssemblyContext:
    def __init__(self, budget, model, agent_id, data):
        self.total_budget = budget
        self.remaining_budget = budget
        self.model = model
        self.agent_id = agent_id
        self.messages = []
        self.element_reports = []
        self.pending_compactions = []
        self.include_stack = []
        self.warnings = []
        self.data = data

    @property
    def message_count(self):
        return len(self.messages)

    @property
    def element_count(self):
        return len(self.element_reports)

    def add(self, element):
        if element.token_count > self.remaining_budget:
            self.warnings.append(
                f"element '{element.name}' uses {element.token_count} tokens but only {self.remaining_budget} remain"
            )

        self.messages.extend(element.messages)
        self.remaining_budget = max(0, self.remaining_budget - element.token_count)
        self.element_reports.append(
            {
                "name": element.name,
                "source": element.source,
                "token_count": element.token_count,
                "preview": element.content_preview,
                "priority": element.priority,
                "named_assembly": None,
            }
        )

    def prompts(self):
        return PyElement(
            "prompts",
            self.data["prompts_source"],
            self.data["prompts_messages"],
            self.data["prompts_token_count"],
            100,
        )

    def skills(self):
        return PyElement(
            "skills",
            self.data["skills_source"],
            self.data["skills_messages"],
            self.data["skills_token_count"],
            75,
        )

    def memory(self):
        return PyElement(
            "memory",
            self.data["memory_source"],
            self.data["memory_messages"],
            self.data["memory_token_count"],
            50,
        )

    def history_recent(self, budget=None, count=None):
        selected_messages = []
        selected_tokens = 0
        messages = self.data["history_messages"]
        per_message_tokens = self.data["history_per_message_tokens"]

        for message, token_count in reversed(list(zip(messages, per_message_tokens))):
            if count is not None and len(selected_messages) >= count:
                break
            if (
                budget is not None
                and selected_messages
                and selected_tokens + token_count > budget
            ):
                break
            selected_tokens += token_count
            selected_messages.append(message)

        selected_messages.reverse()
        return PyElement(
            "history",
            f"{self.data['history_source']} (recent)",
            selected_messages,
            selected_tokens,
            0,
        )

    def history_older(self, budget=None, strategy=None):
        older_end = max(0, len(self.data["history_messages"]) - 2)
        older_messages = list(self.data["history_messages"][:older_end])
        older_tokens = sum(self.data["history_per_message_tokens"][:older_end])

        if budget is not None and older_tokens > budget:
            selected_messages = []
            selected_tokens = 0
            pairs = zip(
                reversed(older_messages),
                reversed(self.data["history_per_message_tokens"][:older_end]),
            )
            for message, token_count in pairs:
                if selected_messages and selected_tokens + token_count > budget:
                    break
                selected_tokens += token_count
                selected_messages.append(message)
            selected_messages.reverse()
            older_messages = selected_messages
            older_tokens = selected_tokens

        if strategy is not None:
            self.pending_compactions.append(
                {
                    "element_name": "history",
                    "strategy": strategy,
                    "budget": budget if budget is not None else self.remaining_budget,
                    "messages": older_messages,
                    "source_range": [0, older_end] if older_end > 0 else None,
                }
            )

        return PyElement(
            "history",
            f"{self.data['history_source']} (older)",
            older_messages,
            older_tokens,
            0,
        )

    def include(self, name):
        if name in self.include_stack:
            chain = " -> ".join(self.include_stack)
            self.warnings.append(f"circular reference: {chain} -> {name}")
            return

        resolved = None
        for level in self.data["search_levels"]:
            candidate = os.path.join(level["path"], "assemblies", f"{name}.py")
            if os.path.exists(candidate):
                resolved = {
                    "level": str(level["level"]).lower(),
                    "path": os.path.realpath(candidate),
                }
                break

        if resolved is None:
            self.warnings.append(f"named assembly '{name}' not found")
            return

        self.include_stack.append(name)
        report_snapshot = len(self.element_reports)

        try:
            with _ORIGINAL_OPEN(resolved["path"], "r", encoding="utf-8") as handle:
                code = handle.read()
        except Exception as error:
            self.include_stack.pop()
            self.warnings.append(f"failed to read assembly '{resolved['path']}': {error}")
            return

        error_kind, error_message = _run_assembly_code(code, resolved["path"], self)
        self.include_stack.pop()

        if error_message is not None:
            self.warnings.append(f"error in named assembly '{name}': {error_message}")

        child_elements = self.element_reports[report_snapshot:]
        del self.element_reports[report_snapshot:]

        if child_elements:
            total_tokens = sum(element["token_count"] for element in child_elements)
            self.element_reports.append(
                {
                    "name": f"assembly:{name}",
                    "source": f"named assembly ({resolved['level']})",
                    "token_count": total_tokens,
                    "preview": None,
                    "priority": 0,
                    "named_assembly": {
                        "assembly_name": name,
                        "resolved_level": resolved["level"],
                        "child_elements": child_elements,
                    },
                }
            )


def _make_assembly_module():
    module = types.ModuleType("swage_context")
    module.AssemblyContext = AssemblyContext
    module.PyElement = PyElement
    return module


def _topology_graph(entry_node_id, nodes, edges, max_fan_out=None, max_concurrency=None, max_total_tokens=None):
    return {
        "entry_node_id": entry_node_id,
        "budget": {
            "max_fan_out": max_fan_out,
            "max_concurrency": max_concurrency,
            "max_total_tokens": max_total_tokens,
        },
        "nodes": nodes,
        "edges": edges,
    }


def _topology_node(node_id, kind, agent_id=None, task_name=None, handler_name=None, output_schema=None, iteration_limit=None):
    return {
        "id": node_id,
        "kind": kind,
        "agent_id": agent_id,
        "task_name": task_name,
        "handler_name": handler_name,
        "output_schema": output_schema,
        "iteration_limit": iteration_limit,
    }


def _topology_edge(edge_id, source_node_id, target_node_id, routing):
    return {
        "id": edge_id,
        "source_node_id": source_node_id,
        "target_node_id": target_node_id,
        "routing": routing,
    }


def _make_topology_module():
    module = types.ModuleType("swage_topology")
    module.graph = _topology_graph
    module.llm_node = lambda node_id, agent_id, task_name, output_schema=None, iteration_limit=None: _topology_node(
        node_id,
        "llm",
        agent_id=agent_id,
        task_name=task_name,
        output_schema=output_schema,
        iteration_limit=iteration_limit,
    )
    module.deterministic_node = lambda node_id, handler_name, output_schema=None, iteration_limit=None: _topology_node(
        node_id,
        "deterministic",
        handler_name=handler_name,
        output_schema=output_schema,
        iteration_limit=iteration_limit,
    )
    module.continuation_node = lambda node_id, task_name, output_schema=None, iteration_limit=None, agent_id=None: _topology_node(
        node_id,
        "continuation",
        agent_id=agent_id,
        task_name=task_name,
        output_schema=output_schema,
        iteration_limit=iteration_limit,
    )
    module.unconditional_edge = lambda edge_id, source_node_id, target_node_id: _topology_edge(
        edge_id, source_node_id, target_node_id, {"type": "unconditional"}
    )
    module.conditional_edge = lambda edge_id, source_node_id, target_node_id, field_path, equals: _topology_edge(
        edge_id,
        source_node_id,
        target_node_id,
        {"type": "condition", "field_path": field_path, "equals": equals},
    )
    module.fan_out_edge = lambda edge_id, source_node_id, target_node_id, items_field: _topology_edge(
        edge_id,
        source_node_id,
        target_node_id,
        {"type": "fan_out", "items_field": items_field},
    )
    module.fork_edge = lambda edge_id, source_node_id, target_node_id: _topology_edge(
        edge_id, source_node_id, target_node_id, {"type": "fork"}
    )
    return module


def _run_assembly_code(code, path, context):
    module = _make_assembly_module()
    allowed_imports = {"swage_context": module}
    namespace = {
        "__builtins__": _restricted_builtins(allowed_imports),
        "__name__": "__main__",
        "swage_context": module,
    }

    try:
        exec(compile(code, path, "exec"), namespace, namespace)
    except SyntaxError:
        return "syntax", traceback.format_exc()
    except BaseException:
        return "runtime", traceback.format_exc()

    assemble = namespace.get("assemble")
    if assemble is None:
        return "runtime", "assembly file does not define an assemble(ctx) function"

    try:
        assemble(context)
    except KeyboardInterrupt:
        return "timeout", "assembly execution interrupted by timeout"
    except BaseException:
        return "runtime", traceback.format_exc()

    return None, None


def _run_topology_code(code, path):
    module = _make_topology_module()
    allowed_imports = {"swage_topology": module}
    namespace = {
        "__builtins__": _restricted_builtins(allowed_imports),
        "__name__": "__main__",
        "swage_topology": module,
    }

    try:
        exec(compile(code, path, "exec"), namespace, namespace)
    except SyntaxError:
        return None, "SyntaxError", traceback.format_exc()
    except BaseException:
        return None, "RuntimeError", traceback.format_exc()

    builder = namespace.get("build_topology")
    if builder is None:
        return None, "RuntimeError", "topology file does not define a build_topology() function"

    try:
        return builder(), None, None
    except KeyboardInterrupt:
        return None, "TimeoutError", "topology execution interrupted by timeout"
    except BaseException:
        return None, "RuntimeError", traceback.format_exc()


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


def _execute_assembly(payload):
    context = AssemblyContext(
        payload["budget"], payload["model"], payload["agent_id"], payload["data"]
    )
    _, error_kind, error_message = None, None, None
    error_kind, error_message = _run_assembly_code(payload["code"], payload["path"], context)
    if error_message is not None:
        return "", error_kind, error_message, None

    return (
        "",
        None,
        None,
        {
            "messages": context.messages,
            "element_reports": context.element_reports,
            "pending_compactions": context.pending_compactions,
            "warnings": context.warnings,
        },
    )


def _execute_topology(payload):
    graph, exception_type, traceback_text = _run_topology_code(
        payload["code"], payload["path"]
    )
    if traceback_text is not None:
        return "", exception_type, traceback_text, None
    return "", None, None, graph


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

    if payload["kind"] == "python_execute":
        stdout, exception_type, traceback_text, result = _execute_python(
            request_id, connection, reader, payload
        )
    elif payload["kind"] == "assembly":
        stdout, exception_type, traceback_text, result = _execute_assembly(payload)
    elif payload["kind"] == "topology":
        stdout, exception_type, traceback_text, result = _execute_topology(payload)
    else:
        raise RuntimeError(f"unknown execute payload kind: {payload['kind']}")

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
