import { PrismaClient } from '@prisma/client'

const prisma = new PrismaClient()

async function main() {
  console.log('Seeding database...')

  // Create forum categories
  const generalCategory = await prisma.forumCategory.upsert({
    where: { slug: 'general' },
    update: {},
    create: {
      name: 'General Discussion',
      description: 'General discussions about Rust and the community',
      slug: 'general',
      order: 1,
    },
  })

  const gameplayCategory = await prisma.forumCategory.upsert({
    where: { slug: 'gameplay' },
    update: {},
    create: {
      name: 'Gameplay',
      description: 'Share strategies, tips, and gameplay discussions',
      slug: 'gameplay',
      order: 2,
    },
  })

  const supportCategory = await prisma.forumCategory.upsert({
    where: { slug: 'support' },
    update: {},
    create: {
      name: 'Support',
      description: 'Get help and support from the community',
      slug: 'support',
      order: 3,
    },
  })

  console.log('Created forum categories:', {
    generalCategory,
    gameplayCategory,
    supportCategory,
  })

  // Create sample products
  const product1 = await prisma.product.upsert({
    where: { id: 'sample-product-1' },
    update: {},
    create: {
      id: 'sample-product-1',
      name: 'Premium Starter Pack',
      description: 'Get started with premium items and resources',
      price: 9.99,
      category: 'Starter Packs',
      stock: 100,
      active: true,
    },
  })

  const product2 = await prisma.product.upsert({
    where: { id: 'sample-product-2' },
    update: {},
    create: {
      id: 'sample-product-2',
      name: 'Cosmetic Bundle',
      description: 'Exclusive cosmetic items for your character',
      price: 4.99,
      category: 'Cosmetics',
      stock: 50,
      active: true,
    },
  })

  console.log('Created sample products:', { product1, product2 })

  console.log('Seeding completed!')
}

main()
  .catch((e) => {
    console.error(e)
    process.exit(1)
  })
  .finally(async () => {
    await prisma.$disconnect()
  })

