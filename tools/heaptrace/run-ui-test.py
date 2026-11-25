import os,pty,fcntl,termios,struct,subprocess,time,select,json,tempfile,pathlib,sys
exe,library=sys.argv[1:]
dir=pathlib.Path(tempfile.mkdtemp(prefix='tmv-run-ui-',dir='/tmp'))/'run.tmvr'
master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',35,140,0,0))
p=subprocess.Popen([exe,'run','--library',library,'--output',str(dir),'--','/usr/bin/sleep','4'],stdin=slave,stdout=slave,stderr=slave,env=dict(os.environ,TERM='xterm-256color'))
os.close(slave)
out=bytearray()
def drain(seconds):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if select.select([master],[],[],.05)[0]:
            try:out.extend(os.read(master,65536))
            except OSError:break
try:
    drain(1);os.write(master,b'TM');drain(.1);os.write(master,b'q');drain(.3);p.wait(timeout=5)
    assert p.returncode==0,p.returncode
    assert '阶梯保持'.encode() not in out,out[-3000:]
    assert '跳转秒数'.encode() not in out,out[-3000:]
    m=json.load(open(dir/'manifest.json'));assert m['state']=='recording',m
    os.kill(m['pid'],0)
    end=time.monotonic()+8
    while time.monotonic()<end:
        m=json.load(open(dir/'manifest.json'))
        if m['state']!='recording':break
        time.sleep(.2)
    assert m['state']=='complete',m
    print('Run UI passed: automatic preload, quitting preserves target, recorder finishes:',dir)
finally:
    if p.poll() is None:p.kill();p.wait()
    os.close(master)
