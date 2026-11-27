window.__ModuleLoader__.load({
  id: 'hello-extension',
  factory(require) {
    const { createElement } = require('react');
    function GreetingPanel() {
      return createElement('p', null,
        'In the terminal, run ', createElement('code', null, '/hello-greet Ada'),
        ' to try the greeting command.'
      );
    }
    return {
      apply(ctx) {
        return ctx.slots.register({ name: 'customize.plugins.detail', id: 'greeting' }, GreetingPanel);
      },
    };
  },
});
