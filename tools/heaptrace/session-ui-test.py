import os,pty,fcntl,termios,struct,subprocess,time,select,sys,re
exe,path=sys.argv[1:]
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,140,0,0))
p=subprocess.Popen([exe,'open',path],stdin=slave,stdout=slave,stderr=slave,env=dict(os.environ,TERM='xterm-256color'))
os.close(slave)
out=bytearray()
def drain(seconds):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if select.select([master],[],[],0.05)[0]:
            try: out.extend(os.read(master,65536))
            except OSError: break
def screen_text():
    return re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', out.decode(errors='replace'))
try:
    drain(.5)
    os.write(master,b'?');drain(.3)
    fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',40,141,0,0));drain(.4)
    assert '会话帮助' in screen_text()
    os.write(master,b'\x1b');drain(.15)
    os.write(master,b'\t?');drain(.3)
    fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',40,140,0,0));drain(.4)
    assert '展开分配栈' in screen_text()
    os.write(master,b'\x1b');drain(.15)
    os.write(master,b'P');drain(.4)
    fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',40,142,0,0));drain(.4)
    assert b'cache_initialize' in out, 'selected Cache stack not visible without t'
    os.write(master,b'\x1b[B');drain(.3)
    fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',40,140,0,0));drain(.4)
    assert b'sessions_initialize' in out, 'sidebar did not follow selection'
    os.write(master,b'\t');drain(.1)
    for keys in [b'\t',b'P',b't',b't',b'\r',b'b',b'[',b']',b'T15\r',b'\x1b[<0;110;37M',b'\x1b[<32;50;37M',b'\x1b[<32;100;37M',b'\x1b[<0;100;37m',b'T9\r',b'M',b'\t',b'b',b'q']:
        os.write(master,keys);drain(.2)
    p.wait(timeout=5)
    assert p.returncode==0,(p.returncode,out[-4000:])
    assert b'Session.buffer' in out,out[-4000:]
    assert b'Cache' in out,out[-4000:]
    assert b'cache_initialize' in out,out[-4000:]
    assert b'RSS' in out,out[-4000:]
    print('PTY session UI passed: layout, allocation treemap, drilldown, blockmap, timeline, quit')
finally:
    if p.poll() is None:p.kill();p.wait()
    os.close(master)
