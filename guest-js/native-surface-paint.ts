import type { VisibleSurfaceBounds } from './native-surface-layout'
import {
  CORNER_CLIP_PATHS,
  cornerPanels,
  outsidePanels,
  type CornerRadii,
  type CornerRadiusStyles,
  type Rect,
} from './native-surface-geometry'
import {
  committedStyleValues,
  MASK_IMAGE_PROPERTY,
  MASK_POSITION_PROPERTY,
  MASK_SIZE_PROPERTY,
  OCCLUDER_ATTRIBUTE,
  restoreOccluder,
  type BackgroundSnapshot,
  type ClippedOccluder,
  type DrilledAncestor,
  type InlinePropertySnapshot,
  type NativeCssSurfaceState,
} from './native-surface-state'

const BACKGROUND_PROPERTIES = [
  'background-color', 'background-image', 'background-position', 'background-size',
  'background-repeat', 'background-origin', 'background-clip', 'background-attachment',
  'background-blend-mode', 'box-sizing', 'padding-top', 'padding-right', 'padding-bottom',
  'padding-left', 'border-top-width', 'border-right-width', 'border-bottom-width',
  'border-left-width', 'border-top-style', 'border-right-style', 'border-bottom-style',
  'border-left-style',
] as const

/**
 * Re-reads the real page styling with the aperture rule temporarily
 * disabled. All writes happen before all reads, and all mirror writes
 * happen after, so one refresh causes at most one style/layout calculation.
 */
export function refreshBackgrounds(state: NativeCssSurfaceState): void {
  for (const ancestor of state.drilled) {
    if (ancestor.element.getAttribute('data-tauri-native-video-hole') === state.owner) {
      ancestor.element.removeAttribute('data-tauri-native-video-hole')
    }
  }
  const backgrounds = state.drilled.map(({ element }) => readBackground(element))
  state.anchorRadii = readCornerRadiusStyles(state.anchor)
  for (const ancestor of state.drilled) {
    ancestor.element.setAttribute('data-tauri-native-video-hole', state.owner)
  }
  for (let index = 0; index < state.drilled.length; index += 1) {
    applyBackground(state.drilled[index], backgrounds[index])
  }
}

export function readBackground(element: HTMLElement): BackgroundSnapshot {
  const computed = getComputedStyle(element)
  return {
    properties: BACKGROUND_PROPERTIES.map((property) => (
      [property, computed.getPropertyValue(property)] as const)),
    borderRadius: computed.borderRadius,
    clipsX: clipsOverflow(computed.overflowX),
    clipsY: clipsOverflow(computed.overflowY),
    borderLeft: cssPixels(computed.borderLeftWidth),
    borderTop: cssPixels(computed.borderTopWidth),
    borderRight: cssPixels(computed.borderRightWidth),
    borderBottom: cssPixels(computed.borderBottomWidth),
    radii: readCornerRadiusStyles(element, computed),
    paintsBackground: hasVisibleBackground(computed),
  }
}

export function applyBackground(ancestor: DrilledAncestor, snapshot: BackgroundSnapshot): void {
  ancestor.paintsBackground = snapshot.paintsBackground
  if (snapshot.paintsBackground && ancestor.panels.length === 0) createBackgroundPanels(ancestor)
  setStyle(ancestor.mirror, 'display', snapshot.paintsBackground ? 'block' : 'none')
  for (const panel of ancestor.panels) {
    for (const [property, value] of snapshot.properties) {
      setStyle(panel.paint, property, value)
    }
    setStyle(panel.paint, 'border-color', 'transparent')
  }
  setStyle(ancestor.mirror, 'border-radius', snapshot.borderRadius)
  ancestor.clipsX = snapshot.clipsX
  ancestor.clipsY = snapshot.clipsY
  ancestor.borderLeft = snapshot.borderLeft
  ancestor.borderTop = snapshot.borderTop
  ancestor.borderRight = snapshot.borderRight
  ancestor.borderBottom = snapshot.borderBottom
  ancestor.radii = snapshot.radii
}

export function createBackgroundPanels(ancestor: DrilledAncestor): void {
  ancestor.panels = Array.from({ length: 8 }, (_, index) => {
    const clip = document.createElement('div')
    const paint = document.createElement('div')
    Object.assign(clip.style, { position: 'absolute', overflow: 'hidden' })
    Object.assign(paint.style, { position: 'absolute', top: '0', left: '0' })
    if (index >= 4) clip.style.clipPath = CORNER_CLIP_PATHS[index - 4]
    clip.append(paint)
    ancestor.mirror.append(clip)
    return { clip, paint }
  })
}

export function commitAncestor(
  ancestor: DrilledAncestor,
  rect: Rect,
  hole: VisibleSurfaceBounds,
  radii: CornerRadii,
): void {
  if (!ancestor.paintsBackground) return
  setStyle(ancestor.mirror, 'transform', `translate3d(${rect.left}px, ${rect.top}px, 0)`)
  setStyle(ancestor.mirror, 'width', `${Math.max(0, rect.width)}px`)
  setStyle(ancestor.mirror, 'height', `${Math.max(0, rect.height)}px`)
  const panels = outsidePanels(rect, hole)
  for (let index = 0; index < 4; index += 1) {
    const panel = ancestor.panels[index]
    const region = panels[index]
    setStyle(panel.clip, 'transform', `translate3d(${region.left}px, ${region.top}px, 0)`)
    setStyle(panel.clip, 'width', `${Math.max(0, region.width)}px`)
    setStyle(panel.clip, 'height', `${Math.max(0, region.height)}px`)
    setStyle(panel.paint, 'transform', `translate3d(${-region.left}px, ${-region.top}px, 0)`)
    setStyle(panel.paint, 'width', `${Math.max(0, rect.width)}px`)
    setStyle(panel.paint, 'height', `${Math.max(0, rect.height)}px`)
  }
  const corners = cornerPanels(hole, radii)
  for (let index = 0; index < 4; index += 1) {
    const panel = ancestor.panels[index + 4]
    const region = corners[index]
    const localLeft = region.left - rect.left
    const localTop = region.top - rect.top
    setStyle(panel.clip, 'transform', `translate3d(${localLeft}px, ${localTop}px, 0)`)
    setStyle(panel.clip, 'width', `${Math.max(0, region.width)}px`)
    setStyle(panel.clip, 'height', `${Math.max(0, region.height)}px`)
    setStyle(panel.paint, 'transform', `translate3d(${-localLeft}px, ${-localTop}px, 0)`)
    setStyle(panel.paint, 'width', `${Math.max(0, rect.width)}px`)
    setStyle(panel.paint, 'height', `${Math.max(0, rect.height)}px`)
  }
}

export function commitOccluder(
  occluder: ClippedOccluder,
  rect: Rect,
  hole: VisibleSurfaceBounds,
): void {
  const right = rect.left + rect.width
  const bottom = rect.top + rect.height
  const left = Math.max(rect.left, hole.left)
  const top = Math.max(rect.top, hole.top)
  const clippedRight = Math.min(right, hole.right)
  const clippedBottom = Math.min(bottom, hole.bottom)
  if (rect.width <= 0 || rect.height <= 0 || clippedRight <= left || clippedBottom <= top) {
    restoreOccluder(occluder)
    return
  }
  let image = ''
  let position = ''
  let size = ''
  for (const panel of outsidePanels(rect, hole)) {
    if (panel.width <= 0 || panel.height <= 0) continue
    const separator = image ? ',' : ''
    image += `${separator}linear-gradient(#000, #000)`
    position += `${separator}${panel.left}px ${panel.top}px`
    size += `${separator}${panel.width}px ${panel.height}px`
  }
  if (!image) {
    image = 'linear-gradient(transparent, transparent)'
    position = '0 0'
    size = '100% 100%'
  }
  if (occluder.element.getAttribute(OCCLUDER_ATTRIBUTE) !== occluder.owner) {
    occluder.element.setAttribute(OCCLUDER_ATTRIBUTE, occluder.owner)
  }
  occluder.active = true
  setProperty(occluder.element, MASK_IMAGE_PROPERTY, image, 'important')
  setProperty(occluder.element, MASK_POSITION_PROPERTY, position, 'important')
  setProperty(occluder.element, MASK_SIZE_PROPERTY, size, 'important')
}

export function hasVisibleBackground(computed: CSSStyleDeclaration): boolean {
  const image = computed.backgroundImage.trim()
  if (image && image !== 'none') return true
  const color = computed.backgroundColor.trim().toLowerCase()
  if (!color || color === 'transparent') return false
  if (/\/\s*0(?:\.0+)?\s*\)$/.test(color)) return false
  if (/rgba\([^)]*,\s*0(?:\.0+)?\s*\)$/.test(color)) return false
  return true
}

export function cssPixels(value: string): number {
  const parsed = Number.parseFloat(value)
  return Number.isFinite(parsed) ? parsed : 0
}

export function readCornerRadiusStyles(
  element: HTMLElement,
  computed = getComputedStyle(element),
): CornerRadiusStyles {
  return [
    computed.borderTopLeftRadius,
    computed.borderTopRightRadius,
    computed.borderBottomRightRadius,
    computed.borderBottomLeftRadius,
  ]
}

export function setStyle(element: HTMLElement, property: string, value: string): void {
  let values = committedStyleValues.get(element)
  if (!values) {
    values = new Map()
    committedStyleValues.set(element, values)
  }
  if (values.get(property) === value) return
  if (element.style.getPropertyValue(property) !== value) element.style.setProperty(property, value)
  values.set(property, value)
}

export function setProperty(element: HTMLElement, property: string, value: string, priority = ''): void {
  if (element.style.getPropertyValue(property) !== value
    || (priority && element.style.getPropertyPriority(property) !== priority)) {
    element.style.setProperty(property, value, priority)
  }
}

export function inlineProperty(element: HTMLElement, property: string): InlinePropertySnapshot {
  return {
    value: element.style.getPropertyValue(property),
    priority: element.style.getPropertyPriority(property),
  }
}

export function restoreInlineProperty(
  element: HTMLElement,
  property: string,
  snapshot: InlinePropertySnapshot,
): void {
  if (snapshot.value) element.style.setProperty(property, snapshot.value, snapshot.priority)
  else element.style.removeProperty(property)
}

function clipsOverflow(value: string): boolean {
  return value === 'hidden' || value === 'clip' || value === 'scroll' || value === 'auto'
}
