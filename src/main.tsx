import React from 'react';
import { createRoot } from 'react-dom/client';
import '@fontsource/noto-sans-tc/400.css';
import '@fontsource/noto-sans-tc/500.css';
import '@fontsource/noto-sans-tc/600.css';
import '@fontsource/noto-sans-tc/latin-700.css';
import './style.css';
import App from './App';
createRoot(document.getElementById('root')!).render(<React.StrictMode><App /></React.StrictMode>);
