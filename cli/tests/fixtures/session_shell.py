"""A session shell whose server lives in another process group."""
import json, os, pathlib, socket, subprocess, sys, time

if '--server' in sys.argv:
    server = socket.socket()
    server.bind(('127.0.0.1', 0))
    server.listen()
    ready = pathlib.Path('shell-server.tmp')
    ready.write_text(json.dumps(dict(pid=os.getpid(), port=server.getsockname()[1])))
    ready.replace('shell-server.json')
    while True:
        time.sleep(1)
else:
    subprocess.Popen([sys.executable, __file__, '--server'], start_new_session=True).wait()
