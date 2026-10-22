# Append to remote_common.py plus capture_body.py's setup, using phase "warm".
try:
    emit('health', get('/manager/api/health'))
    started = time.monotonic()
    while True:
        status = get('/manager/api/catalog/kline-status')
        emit('warm_progress', {'elapsed_s': round(time.monotonic()-started), 'target_history': status['target_history'], 'active_backfills': status['active_backfills'], 'requests': status['requests']})
        assert status['target_history']['last_error'] is None
        if status['target_history']['ready'] and not status['target_history']['loading']:
            break
        assert time.monotonic()-started < 1800
        time.sleep(20)
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session_id+';', readonly=False)
    emit('diagnostic_session_removed', True)
