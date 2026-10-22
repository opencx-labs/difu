"""Foreground auth fixture: no real login, browser, account or user config."""
import os, pty, signal, select, sys, tempfile, time, termios
from pathlib import Path
for mode in ['success','claude-success','failure','cancel','spawn-failure','hup','term']:
    with tempfile.TemporaryDirectory(prefix='difu-auth-') as directory:
        root=Path(directory)
        fake=root/'fake-login'
        fake.write_text('''#!/usr/bin/env python3
import os,sys,time,termios
from pathlib import Path
root=Path(os.environ['DIFU_AUTH_TEST_ROOT'])
a=termios.tcgetattr(0)
assert a[3] & termios.ICANON and a[3] & termios.ECHO
assert os.tcgetpgrp(0) == os.getpgrp()
(root/'ready').write_text(str(os.getpid()))
mode=os.environ['DIFU_AUTH_TEST_MODE']
assert sys.argv[1:] == (['auth','login'] if mode=='claude-success' else ['login','--device-auth'])
if mode=='failure': sys.exit(7)
if mode in ('success','claude-success'):
    assert input() == 'native auth input'
    sys.exit(0)
while True: time.sleep(.02)
''')
        fake.chmod(0o700)
        env=dict(os.environ,DIFU_AUTH_TEST_MODE=mode,DIFU_AUTH_TEST_ROOT=directory)
        pid,master=pty.fork()
        if pid==0: os.execve(sys.argv[1],[sys.argv[1],'--exact','authentication::tests::auth_terminal_child','--nocapture'],env)
        output=b''; reaped=False; sent=False; deadline=time.monotonic()+8
        try:
            while time.monotonic()<deadline:
                if select.select([master],[],[],.01)[0]:
                    try: output+=os.read(master,65536)
                    except OSError: pass
                if (root/'ready').exists() and not sent:
                    sent=True
                    if mode in ('success','claude-success'): os.write(master,b'native auth input\n')
                    elif mode=='cancel': os.write(master,b'\x03')
                    elif mode=='hup': os.kill(pid,signal.SIGHUP)
                    elif mode=='term': os.kill(pid,signal.SIGTERM)
                found,status=os.waitpid(pid,os.WNOHANG)
                if found:
                    reaped=True
                    assert os.waitstatus_to_exitcode(status)==0, (mode,output.decode(errors='replace'))
                    assert (root/'restored').exists(),mode
                    assert output.count(b'\x1b[?1049h')>=2,(mode,'alternate screen not restored')
                    assert output.count(b'\x1b[?2004h')>=2,(mode,'paste mode not restored')
                    break
            assert reaped,(mode,'timed out',output)
        finally:
            os.close(master)
            if not reaped:
                os.kill(pid,signal.SIGKILL);os.waitpid(pid,0)
