"""Local FastAPI fixture for exercising the compiled ExaProp proxy."""

import asyncio

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse, Response, StreamingResponse

app = FastAPI(docs_url=None, redoc_url=None)


@app.get('/health')
async def health():
    return {'status': 'ok'}


@app.head('/health')
async def head_health():
    return Response(headers={'x-backend': 'fastapi'})


@app.get('/items/{item_id}')
async def item(item_id: int, request: Request):
    return {
        'item_id': item_id,
        'raw_query': request.scope['query_string'].decode('ascii'),
        'authorization_received': 'authorization' in request.headers,
    }


@app.post('/echo')
async def echo(request: Request):
    return Response(await request.body(), media_type='application/octet-stream')


@app.get('/cookies')
async def cookies():
    response = JSONResponse({'ok': True})
    response.set_cookie('session', 'synthetic-session')
    response.set_cookie('theme', 'dark')
    return response


@app.get('/status/{status_code}')
async def status(status_code: int):
    if not 400 <= status_code <= 599:
        raise HTTPException(400, 'Use an error status between 400 and 599')
    return JSONResponse({'synthetic_error': True}, status_code=status_code)


@app.get('/delay')
async def delay(milliseconds: int = 200):
    if not 0 <= milliseconds <= 16000:
        raise HTTPException(400, 'Delay must be between 0 and 16000 ms')
    await asyncio.sleep(milliseconds / 1000)
    return {'delayed_ms': milliseconds}


@app.get('/stream')
async def stream(milliseconds: int = 200):
    if not 0 <= milliseconds <= 16000:
        raise HTTPException(400, 'Delay must be between 0 and 16000 ms')

    async def chunks():
        yield b'first-'
        await asyncio.sleep(milliseconds / 1000)
        yield b'last'

    return StreamingResponse(chunks(), media_type='application/octet-stream')


@app.get('/bytes/{size}')
async def sized_body(size: int):
    if not 0 <= size <= 4194305:
        raise HTTPException(400, 'Fixture limit is 4 MiB plus one byte')

    async def chunks():
        remaining = size
        while remaining:
            chunk_size = min(remaining, 65536)
            yield b'x' * chunk_size
            remaining -= chunk_size

    return StreamingResponse(chunks(), media_type='application/octet-stream')
