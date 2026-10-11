// tahoe-view: muestra en una ventana de Linux la pantalla del macOS que corre en tahoe-run.
// WindowServer dibuja en la memoria de vídeo emulada (/dev/shm/tahoe-fb-<id>, ver emu_iokit.inc); al final del archivo, en
// su última página, va una cabecera "TFB1" con ancho, alto, bytes por fila y formato (0 = B, G, R, X por píxel). El cursor
// (por software) está en /dev/shm/tahoe-fbcursor-<id>: aquí solo se marca su posición.
// Uso: tahoe-view [archivo]   (por omisión, el /dev/shm/tahoe-fb-* más reciente; espera a que aparezca)
#include <SDL3/SDL.h>
#include <SDL3/SDL_main.h>
#include <dirent.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

struct fb_hdr { char magic[4]; uint32_t w, h, bpr, fmt, seq; };

static int newest(char* out, size_t n) {
	DIR* d = opendir("/dev/shm");
	if (!d) return 0;
	struct dirent* e; time_t best = 0; int ok = 0;
	while ((e = readdir(d))) {
		if (strncmp(e->d_name, "tahoe-fb-", 9)) continue;
		char p[512]; snprintf(p, sizeof p, "/dev/shm/%s", e->d_name);
		struct stat st;
		if (stat(p, &st) == 0 && st.st_mtime >= best) { best = st.st_mtime; snprintf(out, n, "%s", p); ok = 1; }
	}
	closedir(d);
	return ok;
}

int main(int argc, char** argv) {
	char path[512] = { 0 };
	if (argc > 1) snprintf(path, sizeof path, "%s", argv[1]);
	if (!SDL_Init(SDL_INIT_VIDEO)) { fprintf(stderr, "tahoe-view: SDL_Init: %s\n", SDL_GetError()); return 1; }
	SDL_Window* win = NULL; SDL_Renderer* ren = NULL; SDL_Texture* tex = NULL;
	uint8_t* m = NULL; size_t msize = 0; const struct fb_hdr* h = NULL; int16_t* cur = NULL;
	if (!SDL_CreateWindowAndRenderer("macOS (tahoe-run)", 1280, 800, SDL_WINDOW_RESIZABLE, &win, &ren)) {
		fprintf(stderr, "tahoe-view: ventana: %s\n", SDL_GetError()); return 1;
	}
	SDL_SetRenderVSync(ren, 1);
	for (int run = 1; run;) {
		SDL_Event ev;
		while (SDL_PollEvent(&ev)) if (ev.type == SDL_EVENT_QUIT || (ev.type == SDL_EVENT_KEY_DOWN && ev.key.key == SDLK_F12)) run = 0;
		if (!m) {                                            // esperar a que WindowServer cree la memoria de vídeo
			if (argc > 1 || newest(path, sizeof path)) {
				int fd = open(path, O_RDONLY);
				struct stat st;
				if (fd >= 0 && fstat(fd, &st) == 0 && st.st_size > 4096) {
					msize = (size_t)st.st_size;
					m = mmap(NULL, msize, PROT_READ, MAP_SHARED, fd, 0);
					if (m == MAP_FAILED) m = NULL;
				}
				if (fd >= 0) close(fd);
				if (m) {
					h = (const struct fb_hdr*)(m + msize - 4096);
					if (memcmp(h->magic, "TFB1", 4) || h->fmt != 0 || (size_t)h->bpr * h->h > msize - 4096) { munmap(m, msize); m = NULL; }
					else {
						tex = SDL_CreateTexture(ren, SDL_PIXELFORMAT_XRGB8888, SDL_TEXTUREACCESS_STREAMING, (int)h->w, (int)h->h);
						SDL_SetWindowSize(win, (int)h->w, (int)h->h);
						SDL_SetRenderLogicalPresentation(ren, (int)h->w, (int)h->h, SDL_LOGICAL_PRESENTATION_LETTERBOX);
						fprintf(stderr, "tahoe-view: %s: %ux%u\n", path, h->w, h->h);
						char cp[512]; const char* id = strrchr(path, '-');
						snprintf(cp, sizeof cp, "/dev/shm/tahoe-fbcursor-%s", id ? id + 1 : "x");
						int cfd = open(cp, O_RDONLY);
						if (cfd >= 0) { void* c = mmap(NULL, 4096, PROT_READ, MAP_SHARED, cfd, 0); if (c != MAP_FAILED) cur = c; close(cfd); }
					}
				}
			}
		}
		SDL_SetRenderDrawColor(ren, 0, 0, 0, 255);
		SDL_RenderClear(ren);
		if (m && tex) {
			SDL_UpdateTexture(tex, NULL, m, (int)h->bpr);
			SDL_RenderTexture(ren, tex, NULL, NULL);
			if (cur) {                                           // StdFBShmem_t.cursorLoc (+28): punto de 3×3
				SDL_FRect r = { (float)cur[14] - 1, (float)cur[15] - 1, 3, 3 };
				SDL_SetRenderDrawColor(ren, 255, 255, 255, 255); SDL_RenderFillRect(ren, &r);
			}
		}
		SDL_RenderPresent(ren);
		if (!m) SDL_Delay(500);
	}
	SDL_Quit();
	return 0;
}
