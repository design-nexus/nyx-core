#!/usr/bin/env python3
"""Package and publish versioned shared workspace/design source assets."""
import argparse,gzip,hashlib,json,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser();p.add_argument('--tag',default='v0.1.0');p.add_argument('--publish',action='store_true');a=p.parse_args()
def run(*args,capture=False):
 r=subprocess.run(args,cwd=ROOT,check=True,text=True,stdout=subprocess.PIPE if capture else None);return r.stdout if capture else None
if run('git','status','--porcelain',capture=True).strip():raise SystemExit('Commit source changes first')
name=ROOT.name;revision=run('git','rev-parse','HEAD',capture=True).strip();dist=ROOT/'dist';dist.mkdir(exist_ok=True)
source=dist/(name+'-'+a.tag+'-source.tar.gz')
raw=subprocess.check_output(['git','archive','--format=tar','HEAD'],cwd=ROOT)
with source.open('wb') as out,gzip.GzipFile(filename='',fileobj=out,mode='wb',mtime=0) as stream:stream.write(raw)
assets=[source]
if name=='nyx-design':
 run('python3','tools/package-assets.py');assets.append(dist/('nyx-design-'+revision+'.tar.gz'))
notes=dist/'CHANGELOG.md';notes.write_text('# '+name+' '+a.tag+'\n\nVersioned shared workspace for the Nyx standalone suite.\nCanonical APIs and assets retain their existing behavior. Consumer export revisions\nare pinned separately in the umbrella suite manifest and have identical Rust source.\n\nSource commit: '+revision+'\n')
assets.append(notes)
checksums=dist/'SHA256SUMS';checksums.write_text(''.join(hashlib.sha256(x.read_bytes()).hexdigest()+'  '+x.name+'\n' for x in assets));assets.append(checksums)
if a.publish:run('gh','release','create',a.tag,*map(str,assets),'--repo','design-nexus/'+name,'--target',revision,'--title',name+' '+a.tag,'--notes-file',str(notes))
print(checksums)
