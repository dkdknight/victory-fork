import sys,re
d=open(sys.argv[1],encoding='utf-8',errors='replace').read()
open(sys.argv[2],'w',encoding='utf-8').write(re.sub(r'\x1b\[[0-9;]*m','',d))
