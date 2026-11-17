#ifndef OINK_LINUX_DND_NATIVE_H
#define OINK_LINUX_DND_NATIVE_H
#include "linux_dnd_wire.h"
#include <X11/Xlib.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern struct Report r;
extern Display *inject, *observer;
extern Atom selection_atom;
extern bool is_source;
void require(bool condition, const char *message);
void require_isolated(void);
void native_init(void);
void native_poll(void);
void motion(int x, int y);
void button(bool down);
void escape_key(void);
void held_context(unsigned long allowed_get);
void no_grab(void);
void pointer_at_source(void);
void toolkit_poll(void);
void toolkit_held(int pointer_id);
void toolkit_released(void);
#endif
