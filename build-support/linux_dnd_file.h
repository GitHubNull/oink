#ifndef OINK_LINUX_DND_FILE_H
#define OINK_LINUX_DND_FILE_H
#include "linux_dnd_roles.h"

/* Authorized-root file staging. FILE_READ (source) securely reads a real file
 * below the private root into the payload body; FILE_STAGE (proxy) verifies
 * the received body against its digest and publishes it under staged/, one
 * atomic rename after a verified staging write; FILE_VERIFY (catcher/target)
 * really opens the published path, reads it at offsets (regular file) or
 * walks it deterministically (directory), and reports the SHA-256 of what it
 * actually saw. Paths are relative, escape- and symlink-fenced. */
void file_init(void);
unsigned long file_read(unsigned long seq, const unsigned long *bytes, size_t count);
unsigned long file_stage(unsigned long seq, const unsigned long *bytes, size_t count);
unsigned long file_verify(unsigned long seq, const unsigned long *bytes, size_t count);
#endif
