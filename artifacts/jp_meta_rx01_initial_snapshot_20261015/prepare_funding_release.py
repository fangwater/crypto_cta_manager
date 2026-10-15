"""Add only funding changes to the already verified release source package."""
import difflib
import pathlib
import re
import shutil
import subprocess

ROOT=pathlib.Path(__file__).resolve().parents[2]
STAGE=pathlib.Path(__file__).resolve().parent
OUT=STAGE/'release-source'
assert OUT.is_dir()
for name in ['src/funding.rs','src/funding/rapidx.rs','src/config.rs','src/exchange_leverage.rs',
             'src/nav.rs','src/nav/fill_recovery.rs','src/monitor.rs','migrations/schema.sql',
             'scripts/apply_funding_schema.sql','scripts/apply_funding_settings_schema.sql',
             'config/cta-manager.example.toml']:
    target=OUT/name; target.parent.mkdir(parents=True,exist_ok=True); shutil.copy2(ROOT/name,target)
lib=(ROOT/'src/lib.rs').read_text().replace('pub mod execution_slippage;\n','')
(OUT/'src/lib.rs').write_text(lib)

# Funding hunks only; leave the independent position-creation permissions change
# and execution-slippage work out of this host release.
diff=subprocess.check_output(['git','diff','--','src/web.rs'],cwd=ROOT,text=True)
header=diff[:diff.index('@@')]
hunks=re.split(r'(?=^@@ )',diff[diff.index('@@'):],flags=re.M)
selected=[]
for hunk in hunks:
    if not hunk: continue
    old=int(re.match(r'@@ -(\d+)',hunk).group(1))
    if 700<=old<=1000 or 2100<=old<=2400 or old>=5100: continue
    if old==1550: continue  # clones used only by the unrelated slippage feature
    selected.append(hunk)
baseline=subprocess.check_output(['git','show','HEAD:src/web.rs'],cwd=ROOT,text=True)
(OUT/'src/web.rs').write_text(baseline)
subprocess.run(['patch','--batch','--forward','-p1'],input=header+''.join(selected),cwd=OUT,text=True,check=True)
web=(OUT/'src/web.rs').read_text()
web=web.replace('    pub execution_slippage: crate::execution_slippage::ExecutionSlippage,\n','')
start=web.index('    let execution_slippage = if '); end=web.index('    let funding = ',start)
web=web[:start]+web[end:]
web=web.replace('        execution_slippage,\n','').replace('            execution_slippage: Default::default(),\n','')
block='''        (
            "execution_slippage".to_string(),
            serde_json::to_string(&snapshot.execution_slippage).unwrap(),
        ),
'''
assert block in web; web=web.replace(block,'')
assert 'execution_slippage' not in web and 'position_publish_allowed' not in web
(OUT/'src/web.rs').write_text(web)

for name in ['App.tsx','api.ts','types.ts','nav-dashboard.css','components/NavTimelineChart.tsx',
             'components/AnalysisMetric.tsx','components/FundingPanel.tsx','components/FundingStatusBadge.tsx',
             'components/FundingSettingsPanel.tsx','pages/AccountOverviewPage.tsx','pages/WorkspacePage.tsx',
             'pages/config/AccountBindingsPage.tsx']:
    target=OUT/'frontend/src'/name; target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copy2(ROOT/'frontend/src'/name,target)
app=(OUT/'frontend/src/App.tsx').read_text()
app='\n'.join(line for line in app.split('\n') if 'ExecutionSlippagePanel' not in line)
(OUT/'frontend/src/App.tsx').write_text(app)
types=(OUT/'frontend/src/types.ts').read_text()
types=types.replace('  execution_slippage?: ExecutionSlippage\n','')
start=types.index('export interface ExecutionSlippage {'); end=types.index('export interface FundingSummary {',start)
types=types[:start]+types[end:]
types=types.replace('  /** Session-specific permission returned by the strategy catalog. */\n  can_publish?: boolean\n','')
types=types.replace('  after_fee_shortfall_bps: number | null\n','').replace('  after_fee_shortfall_bps: number\n','')
(OUT/'frontend/src/types.ts').write_text(types)
deps=OUT/'frontend/node_modules'
if not deps.exists(): deps.symlink_to(ROOT/'frontend/node_modules',target_is_directory=True)
files={p.as_posix() for p in OUT.rglob('*') if p.is_file() and 'node_modules' not in p.parts}
changes=[]
for p in sorted(files):
    rel=pathlib.Path(p).relative_to(OUT).as_posix()
    if not (rel.startswith(('src/','frontend/src/','migrations/','scripts/')) or rel=='config/cta-manager.example.toml'): continue
    result=subprocess.run(['git','show','HEAD:'+rel],cwd=ROOT,capture_output=True,text=True)
    previous=result.stdout if result.returncode==0 else ''
    value=pathlib.Path(p).read_text()
    if previous!=value:
        changes.extend(difflib.unified_diff(previous.splitlines(True),value.splitlines(True),fromfile='a/'+rel,tofile='b/'+rel))
(STAGE/'funding_release.patch').write_text(''.join(changes))
print('Prepared funding-only release package and review patch')
