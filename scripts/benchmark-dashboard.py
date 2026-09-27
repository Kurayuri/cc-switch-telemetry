#!/usr/bin/env python3
"""Compare two binaries on caller-supplied isolated SQLite backups (never a live DB)."""
import argparse
import gzip
import json
import math
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import time
from urllib.parse import urlencode
from urllib.request import build_opener, ProxyHandler, Request

HTTP = build_opener(ProxyHandler({}))

def request(origin, path, headers=None):
    started = time.perf_counter()
    with HTTP.open(Request(origin+path, headers=headers or {}), timeout=60) as response:
        data = response.read()
        size = len(data)
        if response.headers.get('Content-Encoding') == 'gzip':
            data = gzip.decompress(data)
        return json.loads(data), time.perf_counter()-started, size, dict(response.headers)

def same(a, b, path='root'):
    if isinstance(a, dict) and isinstance(b, dict):
        assert a.keys() == b.keys(), path
        for key in a: same(a[key], b[key], path+'.'+key)
    elif isinstance(a, list) and isinstance(b, list):
        assert len(a) == len(b), (path, len(a), len(b))
        for index, (x,y) in enumerate(zip(a,b)): same(x,y,f'{path}[{index}]')
    elif isinstance(a, (int,float)) and isinstance(b, (int,float)):
        assert math.isclose(a,b,rel_tol=1e-10,abs_tol=1e-8), (path,a,b)
    else:
        assert a == b, (path,a,b)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('directory', type=Path)
    parser.add_argument('--optimized', type=Path, default=Path('target/release/telemetry-server'))
    args=parser.parse_args()
    work=args.directory.resolve()
    servers=[]
    report={}
    try:
        for label, binary in [('baseline',work/'baseline-server'),('optimized',args.optimized.resolve())]:
            database=work/f'{label}.db'
            assert database.exists(), 'Supply isolated backups first'
            with socket.socket() as sock:
                sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
            origin=f'http://127.0.0.1:{port}'
            env=dict(os.environ,TELEMETRY_DB=str(database),TELEMETRY_LISTEN=f'127.0.0.1:{port}')
            env.pop('ADMIN_PASSWORD',None)
            log=(work/f'{label}.log').open('w')
            started=time.monotonic()
            server=subprocess.Popen([str(binary)],env=env,stdout=log,stderr=log)
            servers.append((server,log))
            for _ in range(900):
                if server.poll() is not None: raise RuntimeError((work/f'{label}.log').read_text())
                try:
                    request(origin,'/healthz');break
                except OSError:time.sleep(.1)
            else: raise TimeoutError('startup')
            report[label]={'origin':origin,'startupSeconds':time.monotonic()-started}
            print(label,'startup',round(report[label]['startupSeconds'],3),flush=True)
        now=int(time.time())
        paths={}
        for days in (1,7,30):
            query=urlencode({'from':now-days*86400,'to':now,'bucket':'auto','tz_offset_minutes':480})
            for endpoint in ('overview','quota'):
                paths[f'{endpoint}-{days}d']='/v3/dashboard/'+endpoint+'?'+query
        paths['current']='/v3/dashboard/quota?include_history=false&'+urlencode({'from':now-86400,'to':now})
        paths['daily']='/v3/dashboard/daily?'+urlencode({'from':now-365*86400,'to':now,'tz_offset_minutes':480})
        paths['resets']='/v3/dashboard/quota/resets'
        paths['all-time']='/v3/dashboard/overview?'+urlencode({'all_time':'true','to':now,'bucket':'auto','tz_offset_minutes':480})
        expected={}
        for name,path in paths.items():
            expected[name],duration,_,_=request(report['baseline']['origin'],path)
            report['baseline'][name]=duration
            result,duration,_,_=request(report['optimized']['origin'],path)
            same(expected[name],result,name)
            report['optimized']['cold-'+name]=duration
            print('equivalent',name,'baseline',round(report['baseline'][name],3),'new',round(duration,3),flush=True)
        # The background projection worker is incremental; measure warm separately.
        started=time.monotonic()
        for iteration in range(180):
            with sqlite3.connect(f'file:{work / "optimized.db"}?mode=ro',uri=True) as db:
                done=db.execute('SELECT revision=processed FROM billing_projection_meta WHERE id=1').fetchone()[0]
                resets=db.execute('SELECT count(*) FROM quota_reset_cache WHERE revision!=cached_revision').fetchone()[0]
                dirty=db.execute("SELECT count(*) FROM usage_cache_partitions WHERE state='dirty' AND hour_start<?",(now-now%3600,)).fetchone()[0]
            if done and not resets and not dirty:break
            if iteration%10==0:print('warming',round(time.monotonic()-started,1),'dirty closed hours',dirty,'reset series',resets,flush=True)
            time.sleep(1)
        else:raise TimeoutError('cache warm-up')
        report['warmupSeconds']=time.monotonic()-started
        for name,path in paths.items():
            timings=[]
            for _ in range(7):
                result,duration,size,headers=request(report['optimized']['origin'],path,{'Accept-Encoding':'gzip'})
                same(expected[name],result,name)
                timings.append(duration)
            report['optimized'][name]={'p95':max(timings),'median':sorted(timings)[3],'bytes':size,'serverTiming':headers.get('server-timing'),'encoding':headers.get('content-encoding')}
            print('warm',name,'p95',round(max(timings),4),'bytes',size,flush=True)
        (work/'report.json').write_text(json.dumps(report,indent=2))
        print('PASS: all responses equivalent; report',work/'report.json',flush=True)
    finally:
        for server,log in servers:
            server.terminate()
            try:server.wait(timeout=10)
            except subprocess.TimeoutExpired:server.kill();server.wait()
            log.close()

if __name__=='__main__':main()
