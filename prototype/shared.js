/* Warden — shared helpers for static prototype (no framework, no build step) */

const DIAL_RADIUS = 70;
const DIAL_STROKE = 8;
const DIAL_R = DIAL_RADIUS - DIAL_STROKE / 2;
const DIAL_CIRCUMFERENCE = 2 * Math.PI * DIAL_R;

/**
 * Renders the circular time-lock dial into `container`.
 * state: 'idle' | 'active' | 'forever'
 * progress: 0..1 fraction of the ring filled
 * timeText / labelText: center copy
 * animate: whether to play the 600ms fill-in (lock activation moment)
 */
function renderDial(container, { state, progress, timeText, labelText, animate = false, size = 160 }) {
  const offset = DIAL_CIRCUMFERENCE * (1 - progress);
  container.classList.add('dial', `state-${state}`);
  if (animate) container.classList.add('animate-fill');
  container.style.width = size + 'px';
  container.style.height = size + 'px';
  container.style.setProperty('--dial-circumference', DIAL_CIRCUMFERENCE);
  container.style.setProperty('--dial-target-offset', offset);

  container.innerHTML = `
    <svg width="${size}" height="${size}" viewBox="0 0 ${DIAL_RADIUS * 2} ${DIAL_RADIUS * 2}">
      <circle class="dial-track" cx="${DIAL_RADIUS}" cy="${DIAL_RADIUS}" r="${DIAL_R}"></circle>
      <circle class="dial-progress" cx="${DIAL_RADIUS}" cy="${DIAL_RADIUS}" r="${DIAL_R}"
        stroke-dasharray="${DIAL_CIRCUMFERENCE}"
        stroke-dashoffset="${animate ? DIAL_CIRCUMFERENCE : offset}"></circle>
    </svg>
    <div class="dial-center">
      <div class="dial-time${timeText.length > 4 ? ' dial-time--word' : ''}">${timeText}</div>
      ${labelText ? `<div class="dial-label">${labelText}</div>` : ''}
    </div>
  `;
}
