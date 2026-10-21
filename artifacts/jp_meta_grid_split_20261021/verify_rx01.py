import collections
import datetime
import json
from pathlib import Path
import subprocess
import time

from templates import ROOT, Session, save

def main():
    session=Session("role='admin'")
    try:
        samples=[]
        for index in range(3):
            response=session.api('/exec_trade06/snapshot')
            st=next(e for e in response['entries'] if e['type']=='exec_pre_trade_state')
            state=st['entry'];rows=state['rows'];now=time.time()*1000
            grouped=collections.defaultdict(list)
            for row in rows:grouped[row['symbol']].append(row)
            differences=[]
            for symbol,items in grouped.items():
                actual=items[0]['account_position_qty']
                allocated=sum(r['current_qty'] for r in items)
                if abs(allocated-actual)>max(1,abs(actual))*1e-8:
                    differences.append({'symbol':symbol,'account_qty':actual,'allocated_qty':allocated,'difference':allocated-actual})
            pending=[{k:r[k] for k in ['strategy_name','symbol','algorithm','target_qty','current_qty','delta_qty','delta_usdt','live_order_qty','pending_qty','active_batches','completion_reason']} for r in rows if not r['execution_complete']]
            summary={'sample':index,'at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'state_ts_ms':state['ts_ms'],'state_age_ms':now-state['ts_ms'],'relay_age_ms':now-st['ts_ms'],'position_ready':state['position_ready'],'rows':len(rows),'strategies':len({r['strategy_name'] for r in rows}),'algorithms':dict(collections.Counter(r['algorithm'] for r in rows)),'completion':dict(collections.Counter(r['completion_reason'] for r in rows)),'cash_rows':sum(not r['symbol'].endswith(('USDT','USDC','USD')) for r in rows),'perpetual_rows':sum(r['symbol'].endswith(('USDT','USDC','USD')) for r in rows),'incomplete_rows':len(pending),'account_allocation_differences':differences}
            samples.append(summary)
            save('rx01_snapshot_'+str(index)+'.json',response)
            save('rx01_pending_'+str(index)+'.json',pending)
            print('STATE',json.dumps(summary),flush=True)
            assert 0<=summary['state_age_ms']<5000
            assert state['position_ready'] and len(rows)>685
            assert all(r['algorithm']=='grid' for r in rows if r['strategy_name']!='SYSTEM_POSITION_CLOSE')
            if index:assert state['ts_ms']>samples[index-1]['state_ts_ms']
            if index<2:time.sleep(10)
        studio=session.api('/manager/api/catalog/accounts/binance_exec_trade06')
        assert not studio['pending_publishes']
        assert all(b['order_strategy_name']=='lgq_grid_spread' for b in studio['bindings'])
        save('rx01_state_verification.json',samples)
        print('RX01_FRESH_STATE_VERIFIED',len(samples),flush=True)
    finally:
        session.close()
        print('TEMP_SESSION_REMOVED')

if __name__=='__main__':main()
