import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as HttpRouter from 'effect/http/HttpRouter';
import * as HttpServer from 'effect/http/HttpServer';
import * as HttpServerResponse from 'effect/http/HttpServerResponse';
import * as Layer from 'effect/Layer';

// An application service read by an HttpRouter route handler. The router, its
// middleware and the web handler are Effect's own code in the bundle; none of
// it reads the application service other than by its key, so the route's read
// is the whole demand and the unread method can go.
const AppStore = Context.Service('test/RealRouterAppStore');
const routerAppUnusedHelper = () => 'unused';
const AppLayer = Layer.effect(
  AppStore,
  Effect.gen(function* () {
    const readOne = () => 'one';
    const routerAppUnused = () => routerAppUnusedHelper();
    return AppStore.of({ readOne, routerAppUnused });
  }),
);

const Routes = HttpRouter.use((router) =>
  Effect.gen(function* () {
    const store = yield* AppStore;
    yield* router.add(
      'GET',
      '/one',
      Effect.sync(() => HttpServerResponse.text(store.readOne())),
    );
  }),
);
const { handler, dispose } = HttpRouter.toWebHandler(
  Routes.pipe(Layer.provide([AppLayer, HttpServer.layerServices])),
  { disableLogger: true },
);

const response = await handler(new Request('http://localhost/one'));
assert.deepStrictEqual([response.status, await response.text()], [200, 'one']);
await dispose();
