import { mount } from 'svelte';

import App from './App.svelte';
import { identify } from './lib/bridge';

identify();

const target = document.getElementById('app');
if (target === null) throw new Error('index.html has no #app element');

export default mount(App, { target });
