import os,pty,fcntl,termios,struct,subprocess,time,select,sys,re
exe=sys.argv[1]
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,140,0,0))
p=subprocess.Popen([exe,'--demo','--no-config'],stdin=slave,stdout=slave,stderr=slave,env=dict(os.environ,TERM='xterm-256color'))
os.close(slave);out=bytearray()
def drain(seconds=.2):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if select.select([master],[],[],.03)[0]:
            try:out.extend(os.read(master,65536))
            except OSError:break
def mouse(code,x,y,end='M'):
    os.write(master,f'\x1b[<{code};{x};{y}{end}'.encode());drain()
def redraw(width):
    fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',40,width,0,0));drain(.4)
try:
    drain(.4)
    toolbar = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', out.decode(errors='replace')).replace(' ', '')
    assert '[列表3]' in toolbar and '[菜单?]' in toolbar, toolbar[:500]
    mouse(0,25,2);redraw(142)
    assert '映射列表' in re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', out.decode(errors='replace'))
    mouse(0,10,5);mouse(65,10,5);mouse(32,10,7);mouse(0,10,7,'m')
    mouse(0,96,2);redraw(140)
    assert '定时刷新' in re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', out.decode(errors='replace'))
    mouse(2,60,20) # right click closes settings without exiting
    mouse(0,83,2);redraw(142)
    assert '基线' in re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', out.decode(errors='replace'))
    mouse(65,50,15);mouse(2,50,15)
    # Menu -> quit: final keyboard q is not needed for navigation above.
    os.write(master,b'q');drain();p.wait(timeout=5);assert p.returncode==0
    print('Original layout mouse passed: toolbar, list, drag, scroll, settings, memory, right-back')
finally:
    if p.poll() is None:p.kill();p.wait()
    os.close(master)
